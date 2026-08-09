//! Command-free Linux kernel and cgroup-v2 prerequisites for the personal-worker runtime.
//!
//! This module deliberately does not earn the cgroup-delegation or kernel-capability runtime
//! evidence classes. Those classes also require a durable delegated-parent identity and the exact
//! successful journaled smoke generation, which belong to R02. The opaque result here is only a
//! current prerequisite that a later same-lock composer may consume.

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
use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const PERSONAL_WORKER_RUNTIME_KERNEL_PREREQUISITE_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const MAX_SMALL_FILE_BYTES: usize = 4_096;
const MAX_FILESYSTEMS_BYTES: usize = 65_536;
const MAX_MOUNTINFO_BYTES: usize = 1_048_576;
const MAX_MOUNTINFO_ROWS: usize = 8_192;
const MAX_MOUNTINFO_LINE_BYTES: usize = 8_192;
const PROC_SUPER_MAGIC: u64 = 0x0000_9fa0;
const SYSFS_MAGIC: u64 = 0x6265_6572;
const CGROUP2_SUPER_MAGIC: u64 = 0x6367_7270;
const DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-kernel-prerequisite-v1";
const REDACTED: &str = "<private-runtime-kernel-prerequisite>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeKernelPrerequisiteDisposition {
    ObservedPrerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeKernelPrerequisiteSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeKernelPrerequisiteDisposition,
    prerequisite_groups: u8,
}

impl PersonalWorkerRuntimeKernelPrerequisiteSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeKernelPrerequisiteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn prerequisite_groups(self) -> u8 {
        self.prerequisite_groups
    }
}

/// Opaque current prerequisites for future cgroup-delegation and kernel-capability evidence.
///
/// This type has no public constructor, serialization, clone, digest accessor, or readiness
/// conversion. Device/inode/timestamp snapshots are used only while observing and are not durable
/// semantic identity.
#[derive(PartialEq, Eq)]
pub struct PersonalWorkerRuntimeKernelPrerequisite {
    summary: PersonalWorkerRuntimeKernelPrerequisiteSummary,
    cgroup_v2: Sha256Digest,
    kernel_namespace: Sha256Digest,
}

impl PersonalWorkerRuntimeKernelPrerequisite {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeKernelPrerequisiteSummary {
        self.summary
    }
}

impl fmt::Debug for PersonalWorkerRuntimeKernelPrerequisite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeKernelPrerequisite")
            .field("summary", &self.summary)
            .field("private_prerequisite", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeKernelPrerequisiteErrorKind {
    IdentityMismatch,
    Missing,
    UnsupportedArchitecture,
    UnsafeFilesystem,
    InvalidEvidence,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeKernelPrerequisiteError {
    pub kind: PersonalWorkerRuntimeKernelPrerequisiteErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for PersonalWorkerRuntimeKernelPrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeKernelPrerequisiteError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeKernelPrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeKernelPrerequisiteError {}

/// Observe bounded Linux kernel and cgroup-v2 prerequisites without invoking a child process.
///
/// # Errors
///
/// Fails closed unless the requested architecture is the running architecture, the fixed procfs,
/// sysfs, and cgroup-v2 bindings are exact, required controllers and namespace policy are present,
/// and every source remains unchanged through final descriptor/path revalidation.
pub fn observe_personal_worker_runtime_kernel_prerequisite(
    project: &ProjectIdentity,
    expected_architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<PersonalWorkerRuntimeKernelPrerequisite, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let observer = (
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    );
    observe_at(
        Path::new("/"),
        (0, 0),
        observer,
        project,
        expected_architecture,
        FilesystemMagic {
            procfs: PROC_SUPER_MAGIC,
            sysfs: SYSFS_MAGIC,
            cgroup2: CGROUP2_SUPER_MAGIC,
        },
        || {},
    )
}

#[derive(Clone, Copy)]
struct FilesystemMagic {
    procfs: u64,
    sysfs: u64,
    cgroup2: u64,
}

#[allow(clippy::too_many_arguments)]
fn observe_at<F>(
    root_path: &Path,
    authority_owner: (u32, u32),
    observer: (u32, u32),
    project: &ProjectIdentity,
    expected_architecture: PersonalWorkerRuntimeArchitecture,
    magic: FilesystemMagic,
    before_revalidation: F,
) -> Result<PersonalWorkerRuntimeKernelPrerequisite, PersonalWorkerRuntimeKernelPrerequisiteError>
where
    F: FnOnce(),
{
    let architecture = host_architecture().ok_or_else(unsupported_error)?;
    if architecture != expected_architecture {
        return Err(identity_error());
    }

    let proc = DirectoryChain::open(root_path, "proc", authority_owner)?;
    require_filesystem_type(proc.leaf(), magic.procfs)?;
    let proc_sys_kernel = DirectoryChain::open(root_path, "proc/sys/kernel", authority_owner)?;
    require_filesystem_type(proc_sys_kernel.leaf(), magic.procfs)?;
    let proc_sys_kernel_random =
        DirectoryChain::open(root_path, "proc/sys/kernel/random", authority_owner)?;
    require_filesystem_type(proc_sys_kernel_random.leaf(), magic.procfs)?;
    let proc_sys_user = DirectoryChain::open(root_path, "proc/sys/user", authority_owner)?;
    require_filesystem_type(proc_sys_user.leaf(), magic.procfs)?;
    let sys = DirectoryChain::open(root_path, "sys", authority_owner)?;
    require_filesystem_type(sys.leaf(), magic.sysfs)?;
    let cgroup = DirectoryChain::open(root_path, "sys/fs/cgroup", authority_owner)?;
    require_filesystem_type(cgroup.leaf(), magic.cgroup2)?;

    let mut boot_id = BoundPseudoFile::open(
        proc_sys_kernel_random.leaf(),
        "boot_id",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut kernel_release = BoundPseudoFile::open(
        proc_sys_kernel.leaf(),
        "osrelease",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut unprivileged_userns = BoundPseudoFile::open(
        proc_sys_kernel.leaf(),
        "unprivileged_userns_clone",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_user_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_user_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_mount_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_mnt_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_pid_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_pid_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_ipc_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_ipc_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_uts_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_uts_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_cgroup_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_cgroup_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut max_network_namespaces = BoundPseudoFile::open(
        proc_sys_user.leaf(),
        "max_net_namespaces",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.procfs,
    )?;
    let mut filesystems = BoundPseudoFile::open(
        proc.leaf(),
        "filesystems",
        authority_owner,
        MAX_FILESYSTEMS_BYTES,
        magic.procfs,
    )?;
    let mut controllers = BoundPseudoFile::open(
        cgroup.leaf(),
        "cgroup.controllers",
        authority_owner,
        MAX_SMALL_FILE_BYTES,
        magic.cgroup2,
    )?;
    let mut proc_self = BoundProcSelf::open(&proc, authority_owner, observer, magic.procfs)?;

    let boot_id_value = parse_boot_id(&boot_id.bytes)?;
    let kernel_release_value = parse_kernel_release(&kernel_release.bytes)?;
    require_decimal(&unprivileged_userns.bytes, Some(1))?;
    let max_user_namespaces_value = require_decimal(&max_user_namespaces.bytes, None)?;
    let max_mount_namespaces_value = require_decimal(&max_mount_namespaces.bytes, None)?;
    let max_pid_namespaces_value = require_decimal(&max_pid_namespaces.bytes, None)?;
    let max_ipc_namespaces_value = require_decimal(&max_ipc_namespaces.bytes, None)?;
    let max_uts_namespaces_value = require_decimal(&max_uts_namespaces.bytes, None)?;
    let max_cgroup_namespaces_value = require_decimal(&max_cgroup_namespaces.bytes, None)?;
    let max_network_namespaces_value = require_decimal(&max_network_namespaces.bytes, None)?;
    require_filesystems(&filesystems.bytes)?;
    let controller_values = require_controllers(&controllers.bytes)?;
    let mount = require_cgroup2_mount(&proc_self.mountinfo.bytes)?;

    let project_binding = project_digest(project);
    let cgroup_sources = vec![
        project_binding.clone(),
        boot_id.policy_digest(b"boot_id"),
        controllers.policy_digest(b"cgroup.controllers"),
        proc_self.mountinfo.policy_digest(b"mountinfo"),
        cgroup.leaf_policy_digest(b"sys/fs/cgroup"),
    ];
    let cgroup_v2 = prerequisite_digest(
        b"cgroup_v2",
        &cgroup_sources,
        &[
            boot_id_value.as_bytes(),
            mount.device.as_bytes(),
            mount.root.as_bytes(),
            mount.mount_point.as_bytes(),
            mount.mount_options.as_bytes(),
            mount.optional_fields.as_bytes(),
            mount.filesystem_type.as_bytes(),
            mount.source.as_bytes(),
            mount.super_options.as_bytes(),
            controller_values.as_bytes(),
            &magic.cgroup2.to_be_bytes(),
        ],
    )?;
    let architecture_tag = [architecture as u8];
    let kernel_sources = vec![
        project_binding,
        boot_id.policy_digest(b"boot_id"),
        kernel_release.policy_digest(b"osrelease"),
        unprivileged_userns.policy_digest(b"unprivileged_userns_clone"),
        max_user_namespaces.policy_digest(b"max_user_namespaces"),
        max_mount_namespaces.policy_digest(b"max_mnt_namespaces"),
        max_pid_namespaces.policy_digest(b"max_pid_namespaces"),
        max_ipc_namespaces.policy_digest(b"max_ipc_namespaces"),
        max_uts_namespaces.policy_digest(b"max_uts_namespaces"),
        max_cgroup_namespaces.policy_digest(b"max_cgroup_namespaces"),
        max_network_namespaces.policy_digest(b"max_net_namespaces"),
        filesystems.policy_digest(b"filesystems"),
        proc.leaf_policy_digest(b"proc"),
        sys.leaf_policy_digest(b"sys"),
    ];
    let kernel_namespace = prerequisite_digest(
        b"kernel_namespace",
        &kernel_sources,
        &[
            boot_id_value.as_bytes(),
            kernel_release_value.as_bytes(),
            &architecture_tag,
            &max_user_namespaces_value.to_be_bytes(),
            &max_mount_namespaces_value.to_be_bytes(),
            &max_pid_namespaces_value.to_be_bytes(),
            &max_ipc_namespaces_value.to_be_bytes(),
            &max_uts_namespaces_value.to_be_bytes(),
            &max_cgroup_namespaces_value.to_be_bytes(),
            &max_network_namespaces_value.to_be_bytes(),
            b"unprivileged_userns_clone=1",
            b"procfs,cgroup2,overlay,sysfs,tmpfs",
            &magic.procfs.to_be_bytes(),
            &magic.sysfs.to_be_bytes(),
        ],
    )?;

    before_revalidation();

    for file in [
        &mut boot_id,
        &mut kernel_release,
        &mut unprivileged_userns,
        &mut max_user_namespaces,
        &mut max_mount_namespaces,
        &mut max_pid_namespaces,
        &mut max_ipc_namespaces,
        &mut max_uts_namespaces,
        &mut max_cgroup_namespaces,
        &mut max_network_namespaces,
        &mut filesystems,
        &mut controllers,
    ] {
        file.revalidate()?;
    }
    proc_self.revalidate(&proc, authority_owner, observer)?;
    for directory in [
        &proc_sys_kernel_random,
        &proc_sys_kernel,
        &proc_sys_user,
        &cgroup,
        &sys,
        &proc,
    ] {
        directory.revalidate()?;
    }
    require_filesystem_type(proc.leaf(), magic.procfs)?;
    require_filesystem_type(sys.leaf(), magic.sysfs)?;
    require_filesystem_type(cgroup.leaf(), magic.cgroup2)?;

    Ok(PersonalWorkerRuntimeKernelPrerequisite {
        summary: PersonalWorkerRuntimeKernelPrerequisiteSummary {
            schema_version: PERSONAL_WORKER_RUNTIME_KERNEL_PREREQUISITE_SCHEMA_VERSION,
            disposition: PersonalWorkerRuntimeKernelPrerequisiteDisposition::ObservedPrerequisite,
            prerequisite_groups: 2,
        },
        cgroup_v2,
        kernel_namespace,
    })
}

struct BorrowedParent(OwnedFd);

struct BoundPseudoFile {
    file: File,
    parent: BorrowedParent,
    name: String,
    bytes: Vec<u8>,
    snapshot: rustix::fs::Stat,
    max_bytes: usize,
    filesystem_magic: u64,
}

impl BoundPseudoFile {
    fn open(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
        max_bytes: usize,
        filesystem_magic: u64,
    ) -> Result<Self, PersonalWorkerRuntimeKernelPrerequisiteError> {
        let parent = fcntl_dupfd_cloexec(parent, 0).map_err(|_| io_error())?;
        let fd = fs::openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(map_open)?;
        let mut file = File::from(fd);
        require_filesystem_type(file.as_fd(), filesystem_magic)?;
        let before = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_pseudo_file(&before, owner)?;
        let first = read_bounded(&mut file, max_bytes)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let second = read_bounded(&mut file, max_bytes)?;
        let after = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_pseudo_file(&after, owner)?;
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
            filesystem_magic,
        })
    }

    fn policy_digest(&self, label: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, label);
        hash_stat_policy(&mut hasher, &self.snapshot);
        hasher.finalize().to_vec()
    }

    fn revalidate(&mut self) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
        let before = fs::fstat(&self.file).map_err(|_| changed_error())?;
        require_filesystem_type(self.file.as_fd(), self.filesystem_magic)
            .map_err(|_| changed_error())?;
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
        let rebound = fs::openat(&self.parent.0, &self.name, FILE_FLAGS, Mode::empty())
            .map_err(|_| changed_error())?;
        require_filesystem_type(rebound.as_fd(), self.filesystem_magic)
            .map_err(|_| changed_error())?;
        let rebound = fs::fstat(&rebound).map_err(|_| changed_error())?;
        if first != self.bytes
            || second != self.bytes
            || !same_snapshot(&self.snapshot, &after)
            || !same_snapshot(&self.snapshot, &path)
            || !same_snapshot(&self.snapshot, &rebound)
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
}

impl DirectoryChain {
    fn open(
        root_path: &Path,
        relative: &str,
        owner: (u32, u32),
    ) -> Result<Self, PersonalWorkerRuntimeKernelPrerequisiteError> {
        let root = fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        let root_stat = fs::fstat(&root).map_err(|_| io_error())?;
        inspect_directory(&root_stat, owner)?;
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
            return Err(invalid_error());
        }
        let components = path.components().collect::<Vec<_>>();
        if components.is_empty() {
            return Err(invalid_error());
        }
        for component in components {
            let name = component.as_os_str().to_str().ok_or_else(invalid_error)?;
            let parent = nodes.last().expect("root node").fd.as_fd();
            let fd = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
            let stat = fs::fstat(&fd).map_err(|_| io_error())?;
            inspect_directory(&stat, owner)?;
            nodes.push(DirectoryNode {
                fd,
                name: Some(name.to_owned()),
                snapshot: stat,
            });
        }
        Ok(Self {
            root_path: root_path.to_owned(),
            nodes,
        })
    }

    fn leaf(&self) -> BorrowedFd<'_> {
        self.nodes.last().expect("nonempty chain").fd.as_fd()
    }

    fn leaf_policy_digest(&self, label: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, label);
        hash_stat_policy(
            &mut hasher,
            &self.nodes.last().expect("nonempty chain").snapshot,
        );
        hasher.finalize().to_vec()
    }

    fn revalidate(&self) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
        for (index, node) in self.nodes.iter().enumerate() {
            let held = fs::fstat(&node.fd).map_err(|_| changed_error())?;
            if !same_snapshot(&node.snapshot, &held) {
                return Err(changed_error());
            }
            if index > 0 {
                let path = fs::statat(
                    &self.nodes[index - 1].fd,
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
        let rebound = fs::fstat(&rebound).map_err(|_| changed_error())?;
        if !same_snapshot(&self.nodes[0].snapshot, &rebound) {
            return Err(changed_error());
        }
        Ok(())
    }
}

struct BoundProcSelf {
    self_link: rustix::fs::Stat,
    self_target: Vec<u8>,
    pid_name: String,
    pid_directory: OwnedFd,
    pid_snapshot: rustix::fs::Stat,
    mountinfo: BoundPseudoFile,
}

impl BoundProcSelf {
    fn open(
        proc: &DirectoryChain,
        link_owner: (u32, u32),
        observer: (u32, u32),
        procfs_magic: u64,
    ) -> Result<Self, PersonalWorkerRuntimeKernelPrerequisiteError> {
        let self_link =
            fs::statat(proc.leaf(), "self", AtFlags::SYMLINK_NOFOLLOW).map_err(map_open)?;
        inspect_proc_self_link(&self_link, link_owner)?;
        let first_target = readlink_bytes(proc.leaf(), "self")?;
        let second_target = readlink_bytes(proc.leaf(), "self")?;
        if first_target != second_target {
            return Err(changed_error());
        }
        let pid_name = std::str::from_utf8(&first_target)
            .map_err(|_| invalid_error())?
            .to_owned();
        let pid = canonical_u64(&pid_name).ok_or_else(invalid_error)?;
        if pid != rustix::process::getpid().as_raw_nonzero().get() as u64 {
            return Err(changed_error());
        }
        let pid_directory =
            fs::openat(proc.leaf(), &pid_name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        let pid_snapshot = fs::fstat(&pid_directory).map_err(|_| io_error())?;
        inspect_directory(&pid_snapshot, observer)?;
        let mountinfo = BoundPseudoFile::open(
            pid_directory.as_fd(),
            "mountinfo",
            observer,
            MAX_MOUNTINFO_BYTES,
            procfs_magic,
        )?;
        Ok(Self {
            self_link,
            self_target: first_target,
            pid_name,
            pid_directory,
            pid_snapshot,
            mountinfo,
        })
    }

    fn revalidate(
        &mut self,
        proc: &DirectoryChain,
        link_owner: (u32, u32),
        observer: (u32, u32),
    ) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
        self.mountinfo.revalidate()?;
        let held = fs::fstat(&self.pid_directory).map_err(|_| changed_error())?;
        let path = fs::statat(proc.leaf(), &self.pid_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed_error())?;
        let self_link = fs::statat(proc.leaf(), "self", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed_error())?;
        let self_target = readlink_bytes(proc.leaf(), "self").map_err(|_| changed_error())?;
        if !same_snapshot(&self.pid_snapshot, &held)
            || !same_snapshot(&self.pid_snapshot, &path)
            || !same_snapshot(&self.self_link, &self_link)
            || self.self_target != self_target
        {
            return Err(changed_error());
        }
        inspect_directory(&held, observer)?;
        inspect_proc_self_link(&self_link, link_owner)?;
        Ok(())
    }
}

struct CgroupMount {
    device: String,
    root: String,
    mount_point: String,
    mount_options: String,
    optional_fields: String,
    filesystem_type: String,
    source: String,
    super_options: String,
}

fn require_cgroup2_mount(
    bytes: &[u8],
) -> Result<CgroupMount, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = canonical_text(bytes)?;
    let lines = input.lines().collect::<Vec<_>>();
    if lines.is_empty()
        || lines.len() > MAX_MOUNTINFO_ROWS
        || lines.iter().any(|line| {
            line.is_empty()
                || line.len() > MAX_MOUNTINFO_LINE_BYTES
                || line.chars().any(char::is_control)
        })
    {
        return Err(invalid_error());
    }
    let mut selected = None;
    for line in lines {
        let fields = line.split(' ').collect::<Vec<_>>();
        if fields.iter().any(|field| field.is_empty()) {
            return Err(invalid_error());
        }
        let separators = fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| (*field == "-").then_some(index))
            .collect::<Vec<_>>();
        if separators.len() != 1 {
            return Err(invalid_error());
        }
        let separator = separators[0];
        if separator < 6 || fields.len() != separator + 4 {
            return Err(invalid_error());
        }
        let mount_point = decode_mountinfo(fields[4])?;
        if mount_point != "/sys/fs/cgroup" {
            continue;
        }
        require_positive_decimal(fields[0])?;
        require_positive_decimal(fields[1])?;
        let device = canonical_device(fields[2])?;
        let root = decode_mountinfo(fields[3])?;
        let mount_options = canonical_options(fields[5])?;
        let optional_fields = fields[6..separator]
            .iter()
            .map(|value| {
                require_mountinfo_token(value)?;
                Ok(*value)
            })
            .collect::<Result<BTreeSet<_>, PersonalWorkerRuntimeKernelPrerequisiteError>>()?;
        if optional_fields.len() != fields[6..separator].len() {
            return Err(invalid_error());
        }
        let optional_fields = optional_fields.into_iter().collect::<Vec<_>>().join(",");
        let filesystem_type = fields[separator + 1];
        require_mountinfo_token(filesystem_type)?;
        let source = decode_mountinfo(fields[separator + 2])?;
        let super_options = canonical_options(fields[separator + 3])?;
        if selected.is_some()
            || root != "/"
            || filesystem_type != "cgroup2"
            || source != "cgroup"
            || !contains_options(
                &mount_options,
                &["nodev", "noexec", "nosuid", "relatime", "rw"],
            )
            || !contains_options(&super_options, &["rw"])
        {
            return Err(invalid_error());
        }
        selected = Some(CgroupMount {
            device,
            root,
            mount_point,
            mount_options,
            optional_fields,
            filesystem_type: filesystem_type.to_owned(),
            source,
            super_options,
        });
    }
    selected.ok_or_else(missing_error)
}

fn require_controllers(
    bytes: &[u8],
) -> Result<String, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = canonical_text(bytes)?;
    let line = input.strip_suffix('\n').ok_or_else(invalid_error)?;
    if line.is_empty() || line.contains('\n') {
        return Err(invalid_error());
    }
    let values = line.split(' ').collect::<Vec<_>>();
    if values.len() > 64
        || values.iter().any(|value| {
            value.is_empty()
                || value.len() > 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        return Err(invalid_error());
    }
    let sorted = values.iter().copied().collect::<BTreeSet<_>>();
    if sorted.len() != values.len()
        || !["cpu", "memory", "pids"]
            .iter()
            .all(|required| sorted.contains(required))
    {
        return Err(invalid_error());
    }
    Ok(sorted.into_iter().collect::<Vec<_>>().join(","))
}

fn require_filesystems(bytes: &[u8]) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = canonical_text(bytes)?;
    let mut names = BTreeSet::new();
    for line in input.lines() {
        let Some((prefix, name)) = line.split_once('\t') else {
            return Err(invalid_error());
        };
        if (prefix != "nodev" && !prefix.is_empty())
            || name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || !names.insert(name)
        {
            return Err(invalid_error());
        }
        if ["proc", "sysfs", "tmpfs", "cgroup2", "overlay"].contains(&name) && prefix != "nodev" {
            return Err(invalid_error());
        }
    }
    if !["proc", "sysfs", "tmpfs", "cgroup2", "overlay"]
        .iter()
        .all(|required| names.contains(required))
    {
        return Err(missing_error());
    }
    Ok(())
}

fn parse_boot_id(bytes: &[u8]) -> Result<String, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = canonical_text(bytes)?;
    let value = input.strip_suffix('\n').ok_or_else(invalid_error)?;
    if value.len() != 36
        || value.bytes().enumerate().any(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte != b'-'
            } else {
                !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte)
            }
        })
    {
        return Err(invalid_error());
    }
    Ok(value.to_owned())
}

fn parse_kernel_release(
    bytes: &[u8],
) -> Result<String, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = canonical_text(bytes)?;
    let value = input.strip_suffix('\n').ok_or_else(invalid_error)?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(invalid_error());
    }
    Ok(value.to_owned())
}

fn require_decimal(
    bytes: &[u8],
    exact: Option<u64>,
) -> Result<u64, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = canonical_text(bytes)?;
    let value = input.strip_suffix('\n').ok_or_else(invalid_error)?;
    let value = canonical_u64(value).ok_or_else(invalid_error)?;
    if value == 0 || exact.is_some_and(|expected| value != expected) {
        return Err(invalid_error());
    }
    Ok(value)
}

fn canonical_text(bytes: &[u8]) -> Result<&str, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let input = std::str::from_utf8(bytes).map_err(|_| invalid_error())?;
    if input.is_empty() || !input.ends_with('\n') || input.contains('\0') || input.contains('\r') {
        return Err(invalid_error());
    }
    Ok(input)
}

fn canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

fn require_positive_decimal(
    value: &str,
) -> Result<u64, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let value = canonical_u64(value).ok_or_else(invalid_error)?;
    if value == 0 {
        return Err(invalid_error());
    }
    Ok(value)
}

fn canonical_device(value: &str) -> Result<String, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let Some((major, minor)) = value.split_once(':') else {
        return Err(invalid_error());
    };
    let major = canonical_u64(major).ok_or_else(invalid_error)?;
    let minor = canonical_u64(minor).ok_or_else(invalid_error)?;
    Ok(format!("{major}:{minor}"))
}

fn decode_mountinfo(value: &str) -> Result<String, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            if bytes[index].is_ascii_control() || bytes[index] == b' ' {
                return Err(invalid_error());
            }
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 3 >= bytes.len() {
            return Err(invalid_error());
        }
        let replacement = match &bytes[index + 1..index + 4] {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(invalid_error()),
        };
        decoded.push(replacement);
        index += 4;
    }
    let decoded = String::from_utf8(decoded).map_err(|_| invalid_error())?;
    if decoded.is_empty() || decoded.chars().any(char::is_control) {
        return Err(invalid_error());
    }
    Ok(decoded)
}

fn canonical_options(value: &str) -> Result<String, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let values = value.split(',').collect::<Vec<_>>();
    if values.is_empty()
        || values.len() > 128
        || values.iter().any(|item| {
            item.is_empty()
                || item.len() > 256
                || !item.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'_' | b'-' | b'.' | b'=' | b':' | b'/')
                })
        })
    {
        return Err(invalid_error());
    }
    let sorted = values.iter().copied().collect::<BTreeSet<_>>();
    if sorted.len() != values.len() {
        return Err(invalid_error());
    }
    Ok(sorted.into_iter().collect::<Vec<_>>().join(","))
}

fn contains_options(observed: &str, required: &[&str]) -> bool {
    let observed = observed.split(',').collect::<BTreeSet<_>>();
    required.iter().all(|value| observed.contains(value))
}

fn require_mountinfo_token(
    value: &str,
) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
    {
        return Err(invalid_error());
    }
    Ok(())
}

fn inspect_directory(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn inspect_pseudo_file(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn inspect_proc_self_link(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_symlink()
        || stat.st_nlink != 1
        || (stat.st_uid, stat.st_gid) != owner
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn require_filesystem_type(
    fd: BorrowedFd<'_>,
    expected: u64,
) -> Result<(), PersonalWorkerRuntimeKernelPrerequisiteError> {
    let stat = fs::fstatfs(fd).map_err(|_| io_error())?;
    if stat.f_type as u64 != expected {
        return Err(unsafe_error());
    }
    Ok(())
}

fn readlink_bytes(
    parent: BorrowedFd<'_>,
    name: &str,
) -> Result<Vec<u8>, PersonalWorkerRuntimeKernelPrerequisiteError> {
    fs::readlinkat(parent, name, Vec::new())
        .map(|value| value.into_bytes())
        .map_err(map_open)
}

fn read_bounded(
    file: &mut File,
    max_bytes: usize,
) -> Result<Vec<u8>, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let mut reader: Take<&mut File> = file.take((max_bytes + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > max_bytes {
        return Err(invalid_error());
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

fn hash_stat_policy(hasher: &mut Sha256, stat: &rustix::fs::Stat) {
    for value in [stat.st_uid, stat.st_gid, stat.st_mode] {
        hash_field(hasher, &value.to_be_bytes());
    }
}

fn host_architecture() -> Option<PersonalWorkerRuntimeArchitecture> {
    match std::env::consts::ARCH {
        "aarch64" => Some(PersonalWorkerRuntimeArchitecture::Aarch64),
        "x86_64" => Some(PersonalWorkerRuntimeArchitecture::X86_64),
        _ => None,
    }
}

fn prerequisite_digest(
    group: &[u8],
    sources: &[Vec<u8>],
    values: &[&[u8]],
) -> Result<Sha256Digest, PersonalWorkerRuntimeKernelPrerequisiteError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, DOMAIN);
    hash_field(&mut hasher, group);
    for source in sources {
        hash_field(&mut hasher, source);
    }
    for value in values {
        hash_field(&mut hasher, value);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| invalid_error())
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

fn map_open(error: Errno) -> PersonalWorkerRuntimeKernelPrerequisiteError {
    match error {
        Errno::NOENT => missing_error(),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => unsafe_error(),
        _ => io_error(),
    }
}

const fn error(
    kind: PersonalWorkerRuntimeKernelPrerequisiteErrorKind,
    code: &'static str,
    message: &'static str,
) -> PersonalWorkerRuntimeKernelPrerequisiteError {
    PersonalWorkerRuntimeKernelPrerequisiteError {
        kind,
        code,
        message,
    }
}

const fn identity_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::IdentityMismatch,
        "runtime_kernel_identity_mismatch",
        "personal worker runtime kernel prerequisite identity does not match",
    )
}

const fn missing_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::Missing,
        "runtime_kernel_prerequisite_missing",
        "personal worker runtime kernel prerequisite is missing",
    )
}

const fn unsupported_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::UnsupportedArchitecture,
        "runtime_kernel_architecture_unsupported",
        "personal worker runtime kernel architecture is unsupported",
    )
}

const fn unsafe_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::UnsafeFilesystem,
        "runtime_kernel_unsafe_filesystem",
        "personal worker runtime kernel prerequisite has unsafe filesystem state",
    )
}

const fn invalid_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::InvalidEvidence,
        "runtime_kernel_invalid_evidence",
        "personal worker runtime kernel prerequisite evidence is invalid",
    )
}

const fn changed_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::ChangedDuringRead,
        "runtime_kernel_changed",
        "personal worker runtime kernel prerequisite changed during observation",
    )
}

const fn io_error() -> PersonalWorkerRuntimeKernelPrerequisiteError {
    error(
        PersonalWorkerRuntimeKernelPrerequisiteErrorKind::Io,
        "runtime_kernel_unavailable",
        "personal worker runtime kernel prerequisite could not be read",
    )
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::process::{getegid, geteuid};

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-runtime-kernel-{label}-{}-{sequence}",
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
        owner: (u32, u32),
        project: ProjectIdentity,
        architecture: PersonalWorkerRuntimeArchitecture,
        magic: FilesystemMagic,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = TempRoot::new(label);
            let owner = (geteuid().as_raw(), getegid().as_raw());
            for relative in [
                "proc",
                "proc/sys",
                "proc/sys/kernel",
                "proc/sys/kernel/random",
                "proc/sys/user",
                "sys",
                "sys/fs",
                "sys/fs/cgroup",
            ] {
                let path = root.path().join(relative);
                std_fs::create_dir(&path).expect("create fixture directory");
                set_mode(&path, 0o755);
            }
            let pid_name = std::process::id().to_string();
            let pid = root.path().join("proc").join(&pid_name);
            std_fs::create_dir(&pid).expect("create proc pid");
            set_mode(&pid, 0o755);
            symlink(&pid_name, root.path().join("proc/self")).expect("create proc self link");

            write_file(
                &root.path().join("proc/sys/kernel/random/boot_id"),
                b"12345678-1234-4abc-8def-1234567890ab\n",
            );
            write_file(
                &root.path().join("proc/sys/kernel/osrelease"),
                b"6.8.0-31-generic\n",
            );
            write_file(
                &root
                    .path()
                    .join("proc/sys/kernel/unprivileged_userns_clone"),
                b"1\n",
            );
            write_file(
                &root.path().join("proc/sys/user/max_user_namespaces"),
                b"65536\n",
            );
            write_file(
                &root.path().join("proc/sys/user/max_mnt_namespaces"),
                b"65536\n",
            );
            for name in [
                "max_pid_namespaces",
                "max_ipc_namespaces",
                "max_uts_namespaces",
                "max_cgroup_namespaces",
                "max_net_namespaces",
            ] {
                write_file(&root.path().join("proc/sys/user").join(name), b"65536\n");
            }
            write_file(
                &root.path().join("proc/filesystems"),
                b"nodev\tsysfs\nnodev\ttmpfs\nnodev\tproc\nnodev\tcgroup2\nnodev\toverlay\n\text4\n",
            );
            write_file(
                &root.path().join("sys/fs/cgroup/cgroup.controllers"),
                b"cpuset cpu io memory hugetlb pids\n",
            );
            write_file(
                &pid.join("mountinfo"),
                b"25 1 0:21 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n36 1 0:32 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw,nsdelegate,memory_recursiveprot\n",
            );

            let root_fd = fs::open(root.path(), DIRECTORY_FLAGS, Mode::empty()).expect("open root");
            let host_magic = fs::fstatfs(root_fd).expect("fixture statfs").f_type as u64;
            Self {
                root,
                owner,
                project: ProjectIdentity {
                    repository: "teamleaderleo/smolrunner".to_owned(),
                    runner_scope: RunnerScope::Repository,
                    runner_user: "smolrunner-runner".to_owned(),
                },
                architecture: host_architecture().expect("supported test architecture"),
                magic: FilesystemMagic {
                    procfs: host_magic,
                    sysfs: host_magic,
                    cgroup2: host_magic,
                },
            }
        }

        fn observe(
            &self,
        ) -> Result<
            PersonalWorkerRuntimeKernelPrerequisite,
            PersonalWorkerRuntimeKernelPrerequisiteError,
        > {
            observe_at(
                self.root.path(),
                self.owner,
                self.owner,
                &self.project,
                self.architecture,
                self.magic,
                || {},
            )
        }
    }

    #[test]
    fn observes_only_prerequisite_groups_and_redacts_private_identity() {
        let fixture = Fixture::new("ready");
        let evidence = fixture.observe().expect("observe fixture");
        assert_eq!(
            evidence.summary(),
            PersonalWorkerRuntimeKernelPrerequisiteSummary {
                schema_version: 1,
                disposition:
                    PersonalWorkerRuntimeKernelPrerequisiteDisposition::ObservedPrerequisite,
                prerequisite_groups: 2,
            }
        );
        let debug = format!("{evidence:?}");
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("12345678"));
        assert!(!debug.contains("6.8.0"));
        assert!(!debug.contains("/sys/fs/cgroup"));
    }

    #[test]
    fn rejects_wrong_architecture_before_filesystem_access() {
        let fixture = Fixture::new("architecture");
        let other = match fixture.architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => PersonalWorkerRuntimeArchitecture::X86_64,
            PersonalWorkerRuntimeArchitecture::X86_64 => PersonalWorkerRuntimeArchitecture::Aarch64,
        };
        std_fs::remove_dir_all(fixture.root.path()).expect("remove fixture");
        let error = observe_at(
            fixture.root.path(),
            fixture.owner,
            fixture.owner,
            &fixture.project,
            other,
            fixture.magic,
            || {},
        )
        .expect_err("wrong architecture must fail first");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::IdentityMismatch
        );
    }

    #[test]
    fn rejects_missing_controller_and_invalid_namespace_policy() {
        let fixture = Fixture::new("policy");
        write_file(
            &fixture.root.path().join("sys/fs/cgroup/cgroup.controllers"),
            b"cpu memory\n",
        );
        assert_eq!(
            fixture.observe().expect_err("pids is required").kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::InvalidEvidence
        );
        write_file(
            &fixture.root.path().join("sys/fs/cgroup/cgroup.controllers"),
            b"cpu memory pids\n",
        );
        write_file(
            &fixture
                .root
                .path()
                .join("proc/sys/kernel/unprivileged_userns_clone"),
            b"0\n",
        );
        assert_eq!(
            fixture
                .observe()
                .expect_err("unprivileged namespaces are required")
                .kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::InvalidEvidence
        );

        write_file(
            &fixture
                .root
                .path()
                .join("proc/sys/kernel/unprivileged_userns_clone"),
            b"1\n",
        );
        write_file(
            &fixture.root.path().join("proc/sys/user/max_net_namespaces"),
            b"0\n",
        );
        assert_eq!(
            fixture
                .observe()
                .expect_err("network namespaces are required even for network none")
                .kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::InvalidEvidence
        );
    }

    #[test]
    fn rejects_wrong_filesystem_magic_and_mount_shape() {
        let fixture = Fixture::new("mount");
        let mut wrong_magic = fixture.magic;
        wrong_magic.cgroup2 = wrong_magic.cgroup2.wrapping_add(1);
        let error = observe_at(
            fixture.root.path(),
            fixture.owner,
            fixture.owner,
            &fixture.project,
            fixture.architecture,
            wrong_magic,
            || {},
        )
        .expect_err("wrong filesystem magic must fail");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::UnsafeFilesystem
        );

        let mountinfo = fixture
            .root
            .path()
            .join(format!("proc/{}/mountinfo", std::process::id()));
        write_file(
            &mountinfo,
            b"36 1 0:32 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup ro\n",
        );
        assert_eq!(
            fixture.observe().expect_err("read-only cgroup mount").kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::InvalidEvidence
        );
    }

    #[test]
    fn unrelated_sibling_activity_does_not_change_semantic_identity() {
        let fixture = Fixture::new("stable");
        let first = fixture.observe().expect("first observation");
        let sibling = fixture.root.path().join("proc/unrelated");
        std_fs::create_dir(&sibling).expect("create unrelated sibling");
        set_mode(&sibling, 0o755);
        let second = fixture.observe().expect("second observation");
        assert_eq!(first, second);
    }

    #[test]
    fn boot_or_kernel_change_changes_private_identity() {
        let fixture = Fixture::new("generation");
        let first = fixture.observe().expect("first observation");
        write_file(
            &fixture.root.path().join("proc/sys/kernel/random/boot_id"),
            b"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee\n",
        );
        let second = fixture.observe().expect("second observation");
        assert_ne!(first, second);
        write_file(
            &fixture.root.path().join("proc/sys/kernel/osrelease"),
            b"6.8.0-32-generic\n",
        );
        let third = fixture.observe().expect("third observation");
        assert_ne!(second, third);
    }

    #[test]
    fn final_revalidation_rejects_same_path_file_replacement() {
        let fixture = Fixture::new("rebind");
        let controllers = fixture.root.path().join("sys/fs/cgroup/cgroup.controllers");
        let replacement = fixture.root.path().join("controllers.replacement");
        write_file(&replacement, b"cpu memory pids\n");
        let error = observe_at(
            fixture.root.path(),
            fixture.owner,
            fixture.owner,
            &fixture.project,
            fixture.architecture,
            fixture.magic,
            || {
                std_fs::rename(&replacement, &controllers).expect("replace controllers");
            },
        )
        .expect_err("path replacement must fail");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeKernelPrerequisiteErrorKind::ChangedDuringRead
        );
    }

    #[test]
    fn strict_parsers_reject_aliases_duplicates_and_noncanonical_values() {
        assert!(
            require_cgroup2_mount(
                b"36 1 0:32 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw\n"
            )
            .is_ok()
        );
        assert!(require_cgroup2_mount(b"36 1 0:32 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw\n37 1 0:33 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw\n").is_err());
        assert!(require_controllers(b"cpu memory pids pids\n").is_err());
        assert!(require_decimal(b"01\n", None).is_err());
        assert!(parse_boot_id(b"12345678-1234-4ABC-8def-1234567890ab\n").is_err());
        assert!(decode_mountinfo("/sys/fs/cgroup\\777").is_err());
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        std_fs::write(path, bytes).expect("write fixture file");
        set_mode(path, 0o644);
    }

    fn set_mode(path: &Path, mode: u32) {
        std_fs::set_permissions(path, std_fs::Permissions::from_mode(mode))
            .expect("set fixture mode");
    }
}
