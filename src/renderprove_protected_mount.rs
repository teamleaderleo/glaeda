use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, AtFlags, FileType, Gid, Mode, OFlags, Uid};
use rustix::io::{self, Errno};
use rustix::mount::{self, MoveMountFlags, OpenTreeFlags, UnmountFlags};
use rustix::rand::{GetRandomFlags, getrandom};
use serde::Serialize;

use crate::descriptor_bound_launcher::ReviewedFilesystemIdentity;
use crate::renderprove_native_probe::{
    RENDERPROVE_PROTECTED_MOUNT_ROOT, RenderproveProtectedMountReceipt,
};
use crate::renderprove_verification::{RenderproveSourceIdentity, RenderproveVerificationRequest};

pub const RENDERPROVE_PROTECTED_MOUNT_LEASE_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const MUTABLE_DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const ALIAS_RANDOM_BYTES: usize = 16;
const MAX_ALIAS_ATTEMPTS: usize = 8;

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveProtectedMountSource {
    source: RenderproveSourceIdentity,
    project_path: PathBuf,
    project_identity: ReviewedFilesystemIdentity,
}

impl RenderproveProtectedMountSource {
    /// Bind an already reviewed private project path to its exact source and filesystem identity.
    ///
    /// This type performs no filesystem operation. Acquisition later reopens the path through
    /// no-follow descriptor traversal and requires the exact supplied identity before cloning a
    /// mount from the held descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error unless the project path is absolute, normalized, non-root, and UTF-8.
    pub fn new(
        source: RenderproveSourceIdentity,
        project_path: impl Into<PathBuf>,
        project_identity: ReviewedFilesystemIdentity,
    ) -> Result<Self, RenderproveProtectedMountError> {
        let project_path = validate_absolute_path(project_path.into())?;
        Ok(Self {
            source,
            project_path,
            project_identity,
        })
    }

    #[must_use]
    pub const fn source(&self) -> &RenderproveSourceIdentity {
        &self.source
    }
}

impl fmt::Debug for RenderproveProtectedMountSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveProtectedMountSource")
            .field("source", &self.source)
            .field("project_path", &"<private reviewed path>")
            .field("project_identity", &"<private exact filesystem identity>")
            .finish()
    }
}

pub struct RenderproveProtectedMountLease {
    receipt: RenderproveProtectedMountReceipt,
    mount_root: OwnedFd,
    _project_source: OwnedFd,
    _evidence_source: OwnedFd,
    project_alias_base: OwnedFd,
    evidence_alias_base: OwnedFd,
    _project_alias_mount: OwnedFd,
    _evidence_alias_mount: OwnedFd,
    project_alias_name: OsString,
    evidence_alias_name: OsString,
    project_alias_path: PathBuf,
    evidence_alias_path: PathBuf,
    cleanup_complete: bool,
}

impl RenderproveProtectedMountLease {
    #[must_use]
    pub const fn receipt(&self) -> &RenderproveProtectedMountReceipt {
        &self.receipt
    }

    /// Detach both protected mounts and remove only the broker-created empty aliases.
    ///
    /// Cleanup is attempted in reverse acquisition order. Every step is best-effort, while the first
    /// bounded failure is retained. A cleanup receipt is returned only when both mounts were detached,
    /// both underlying alias directories still match the retained descriptors, and both aliases were
    /// removed.
    ///
    /// # Errors
    ///
    /// Returns a bounded cleanup error without exposing private paths or operating-system prose.
    pub fn cleanup(
        mut self,
    ) -> Result<RenderproveProtectedMountCleanupReceipt, RenderproveProtectedMountError> {
        self.cleanup_internal()?;
        self.cleanup_complete = true;
        Ok(RenderproveProtectedMountCleanupReceipt {
            schema_version: RENDERPROVE_PROTECTED_MOUNT_LEASE_SCHEMA_VERSION,
            project_detached: true,
            evidence_detached: true,
            aliases_removed: true,
        })
    }

    fn cleanup_internal(&mut self) -> Result<(), RenderproveProtectedMountError> {
        let mut first_error = None;
        for path in [&self.evidence_alias_path, &self.project_alias_path] {
            if let Err(error) = detach_mount(path) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = remove_alias_directory(
            &self.mount_root,
            &self.evidence_alias_name,
            &self.evidence_alias_base,
        ) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = remove_alias_directory(
            &self.mount_root,
            &self.project_alias_name,
            &self.project_alias_base,
        ) {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for RenderproveProtectedMountLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveProtectedMountLease")
            .field("receipt", &self.receipt)
            .field("mount_root", &"<private retained descriptor>")
            .field("sources", &"<private retained descriptors>")
            .field("aliases", &"<private retained mount descriptors>")
            .field("cleanup_complete", &self.cleanup_complete)
            .finish()
    }
}

impl Drop for RenderproveProtectedMountLease {
    fn drop(&mut self) {
        if !self.cleanup_complete {
            let _ = self.cleanup_internal();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveProtectedMountCleanupReceipt {
    schema_version: u8,
    project_detached: bool,
    evidence_detached: bool,
    aliases_removed: bool,
}

impl RenderproveProtectedMountCleanupReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn project_detached(&self) -> bool {
        self.project_detached
    }

    #[must_use]
    pub const fn evidence_detached(&self) -> bool {
        self.evidence_detached
    }

    #[must_use]
    pub const fn aliases_removed(&self) -> bool {
        self.aliases_removed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveProtectedMountErrorKind {
    InvalidSource,
    IdentityMismatch,
    UnsafeFilesystem,
    MissingMountRoot,
    MountUnavailable,
    PermissionDenied,
    AliasCollision,
    CleanupFailed,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveProtectedMountError {
    kind: RenderproveProtectedMountErrorKind,
    stage: &'static str,
    message: &'static str,
}

impl RenderproveProtectedMountError {
    const fn new(
        kind: RenderproveProtectedMountErrorKind,
        stage: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            stage,
            message,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RenderproveProtectedMountErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Debug for RenderproveProtectedMountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveProtectedMountError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for RenderproveProtectedMountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for RenderproveProtectedMountError {}

/// Acquire one non-cloneable descriptor-bound project/evidence mount lease.
///
/// The project path is reopened component-by-component from `/` without following symbolic aliases
/// and must match the exact reviewed filesystem identity. The requested evidence directory is opened
/// or created descriptor-relatively beneath that held project directory. Linux `open_tree` clones the
/// two held directory objects and `move_mount` attaches them beneath SmolRunner's fixed root-owned
/// runtime directory using broker-generated names. A receipt is issued only after both aliases reopen
/// to the exact source identities.
///
/// # Errors
///
/// Returns a bounded error for source/evidence drift, unsafe filesystem objects, missing or unsafe
/// mount root, unavailable mount syscalls, insufficient mount authority, alias exhaustion, or partial
/// acquisition failure. The function never falls back to path-based bind mounts.
pub fn acquire_renderprove_protected_mount_lease(
    request: &RenderproveVerificationRequest,
    source: RenderproveProtectedMountSource,
) -> Result<RenderproveProtectedMountLease, RenderproveProtectedMountError> {
    if request.source() != source.source() {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::InvalidSource,
            "source",
            "reviewed project source does not match the Renderprove request",
        ));
    }

    let mount_root_path = Path::new(RENDERPROVE_PROTECTED_MOUNT_ROOT);
    let mount_root = open_absolute_directory(mount_root_path, "mount_root").map_err(|error| {
        match error.kind {
            RenderproveProtectedMountErrorKind::UnsafeFilesystem => error,
            _ => RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::MissingMountRoot,
                "mount_root",
                "Renderprove protected mount root is unavailable",
            ),
        }
    })?;
    require_root_owned_mount_root(&mount_root)?;

    let project_source = open_absolute_directory(&source.project_path, "project")?;
    let project_identity = inspect_directory_identity(&project_source, "project")?;
    if project_identity != source.project_identity {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::IdentityMismatch,
            "project",
            "held project directory does not match the exact reviewed identity",
        ));
    }

    let evidence_source =
        prepare_relative_directory(&project_source, request.evidence().directory())?;
    let evidence_identity = inspect_directory_identity(evidence_source.directory(), "evidence")?;
    if evidence_identity == project_identity {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "evidence directory must be a strict child of the reviewed project",
        ));
    }

    let project_alias = create_alias_directory(&mount_root, "project")?;
    let evidence_alias = match create_alias_directory(&mount_root, "evidence") {
        Ok(alias) => alias,
        Err(error) => {
            let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
            return Err(error);
        }
    };

    let mut operations = LinuxMountOperations;
    if let Err(error) = attach_mount_pair(
        &mut operations,
        &project_source,
        evidence_source.directory(),
        &mount_root,
        &project_alias.name,
        &evidence_alias.name,
        &project_alias.path,
    ) {
        let _ = remove_alias_directory(&mount_root, &evidence_alias.name, &evidence_alias.base);
        let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
        return Err(error);
    }

    let project_alias_mount = match open_alias_mount(
        &mount_root,
        &project_alias.name,
        &project_identity,
        "project_alias",
    ) {
        Ok(alias) => alias,
        Err(error) => {
            let _ = detach_mount(&evidence_alias.path);
            let _ = detach_mount(&project_alias.path);
            let _ = remove_alias_directory(&mount_root, &evidence_alias.name, &evidence_alias.base);
            let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
            return Err(error);
        }
    };
    let evidence_alias_mount = match open_alias_mount(
        &mount_root,
        &evidence_alias.name,
        &evidence_identity,
        "evidence_alias",
    ) {
        Ok(alias) => alias,
        Err(error) => {
            let _ = detach_mount(&evidence_alias.path);
            let _ = detach_mount(&project_alias.path);
            let _ = remove_alias_directory(&mount_root, &evidence_alias.name, &evidence_alias.base);
            let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
            return Err(error);
        }
    };

    let receipt = RenderproveProtectedMountReceipt::from_broker(
        request.source().clone(),
        project_alias.path.clone(),
        project_identity,
        evidence_alias.path.clone(),
        evidence_identity,
        request.evidence().directory().to_path_buf(),
    );

    let evidence_source = evidence_source.commit();
    Ok(RenderproveProtectedMountLease {
        receipt,
        mount_root,
        _project_source: project_source,
        _evidence_source: evidence_source,
        project_alias_base: project_alias.base,
        evidence_alias_base: evidence_alias.base,
        _project_alias_mount: project_alias_mount,
        _evidence_alias_mount: evidence_alias_mount,
        project_alias_name: project_alias.name,
        evidence_alias_name: evidence_alias.name,
        project_alias_path: project_alias.path,
        evidence_alias_path: evidence_alias.path,
        cleanup_complete: false,
    })
}

struct CreatedAlias {
    name: OsString,
    path: PathBuf,
    base: OwnedFd,
}

struct PendingAlias<'a> {
    mount_root: &'a OwnedFd,
    name: &'a OsStr,
    armed: bool,
}

impl<'a> PendingAlias<'a> {
    fn new(mount_root: &'a OwnedFd, name: &'a OsStr) -> Self {
        Self {
            mount_root,
            name,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingAlias<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.mount_root.as_fd(), self.name, AtFlags::REMOVEDIR);
        }
    }
}

fn create_alias_directory(
    mount_root: &OwnedFd,
    prefix: &str,
) -> Result<CreatedAlias, RenderproveProtectedMountError> {
    create_alias_directory_with_generator(mount_root, prefix, || generate_alias_name(prefix))
}

fn create_alias_directory_with_generator(
    mount_root: &OwnedFd,
    prefix: &str,
    mut generate: impl FnMut() -> Result<OsString, RenderproveProtectedMountError>,
) -> Result<CreatedAlias, RenderproveProtectedMountError> {
    for _ in 0..MAX_ALIAS_ATTEMPTS {
        let name = generate()?;
        if !valid_alias_name(prefix, &name) {
            return Err(RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                "alias",
                "generated Renderprove mount alias is invalid",
            ));
        }
        match fs::mkdirat(mount_root.as_fd(), &name, PRIVATE_DIRECTORY_MODE) {
            Ok(()) => {
                let mut pending = PendingAlias::new(mount_root, &name);
                let base = fs::openat(
                    mount_root.as_fd(),
                    &name,
                    MUTABLE_DIRECTORY_FLAGS,
                    Mode::empty(),
                )
                .map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "created Renderprove mount alias could not be retained",
                    )
                })?;
                fs::fchmod(&base, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "created Renderprove mount alias permissions could not be fixed",
                    )
                })?;
                inspect_directory_identity(&base, "alias")?;
                let root_stat = fs::fstat(mount_root).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "Renderprove mount root identity could not be inspected",
                    )
                })?;
                let base_stat = fs::fstat(&base).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "Renderprove mount alias identity could not be inspected",
                    )
                })?;
                if base_stat.st_uid != root_stat.st_uid
                    || base_stat.st_gid != root_stat.st_gid
                    || base_stat.st_mode & 0o7777 != 0o700
                {
                    return Err(RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                        "alias",
                        "Renderprove mount alias ownership or mode is unsafe",
                    ));
                }
                pending.disarm();
                drop(pending);
                return Ok(CreatedAlias {
                    path: Path::new(RENDERPROVE_PROTECTED_MOUNT_ROOT).join(&name),
                    name,
                    base,
                });
            }
            Err(Errno::EXIST) => continue,
            Err(Errno::ACCESS | Errno::PERM) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::PermissionDenied,
                    "alias",
                    "Renderprove mount alias could not be created with current authority",
                ));
            }
            Err(_) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    "alias",
                    "Renderprove mount alias could not be created",
                ));
            }
        }
    }
    Err(RenderproveProtectedMountError::new(
        RenderproveProtectedMountErrorKind::AliasCollision,
        "alias",
        "Renderprove mount alias allocation was exhausted",
    ))
}

fn generate_alias_name(prefix: &str) -> Result<OsString, RenderproveProtectedMountError> {
    let mut random = [0_u8; ALIAS_RANDOM_BYTES];
    let filled = getrandom(&mut random, GetRandomFlags::empty()).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "alias_random",
            "operating-system randomness is unavailable for a Renderprove mount alias",
        )
    })?;
    if filled != random.len() {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "alias_random",
            "operating-system randomness returned an incomplete Renderprove mount alias",
        ));
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(prefix.len() + 1 + random.len() * 2);
    name.push_str(prefix);
    name.push('-');
    for byte in random {
        name.push(HEX[(byte >> 4) as usize] as char);
        name.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(name.into())
}

fn valid_alias_name(prefix: &str, value: &OsStr) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    value.len() == prefix.len() + 1 + ALIAS_RANDOM_BYTES * 2
        && value.starts_with(prefix)
        && value.as_bytes().get(prefix.len()) == Some(&b'-')
        && value[prefix.len() + 1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

trait MountOperations {
    type DetachedMount;

    fn clone_mount(
        &mut self,
        source: &OwnedFd,
    ) -> Result<Self::DetachedMount, RenderproveProtectedMountError>;

    fn attach_mount(
        &mut self,
        detached: &Self::DetachedMount,
        mount_root: &OwnedFd,
        alias: &OsStr,
    ) -> Result<(), RenderproveProtectedMountError>;

    fn detach_mount(&mut self, alias_path: &Path) -> Result<(), RenderproveProtectedMountError>;
}

struct LinuxMountOperations;

impl MountOperations for LinuxMountOperations {
    type DetachedMount = OwnedFd;

    fn clone_mount(
        &mut self,
        source: &OwnedFd,
    ) -> Result<Self::DetachedMount, RenderproveProtectedMountError> {
        mount::open_tree(
            source.as_fd(),
            "",
            OpenTreeFlags::OPEN_TREE_CLONE
                | OpenTreeFlags::OPEN_TREE_CLOEXEC
                | OpenTreeFlags::AT_EMPTY_PATH,
        )
        .map_err(map_mount_error)
    }

    fn attach_mount(
        &mut self,
        detached: &Self::DetachedMount,
        mount_root: &OwnedFd,
        alias: &OsStr,
    ) -> Result<(), RenderproveProtectedMountError> {
        mount::move_mount(
            detached.as_fd(),
            "",
            mount_root.as_fd(),
            alias,
            MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
        )
        .map_err(map_mount_error)
    }

    fn detach_mount(&mut self, alias_path: &Path) -> Result<(), RenderproveProtectedMountError> {
        detach_mount(alias_path)
    }
}

fn attach_mount_pair<B: MountOperations>(
    backend: &mut B,
    project_source: &OwnedFd,
    evidence_source: &OwnedFd,
    mount_root: &OwnedFd,
    project_alias: &OsStr,
    evidence_alias: &OsStr,
    project_alias_path: &Path,
) -> Result<(), RenderproveProtectedMountError> {
    let project_mount = backend.clone_mount(project_source)?;
    backend.attach_mount(&project_mount, mount_root, project_alias)?;
    let evidence_mount = match backend.clone_mount(evidence_source) {
        Ok(mount) => mount,
        Err(error) => {
            let _ = backend.detach_mount(project_alias_path);
            return Err(error);
        }
    };
    if let Err(error) = backend.attach_mount(&evidence_mount, mount_root, evidence_alias) {
        let _ = backend.detach_mount(project_alias_path);
        return Err(error);
    }
    Ok(())
}

fn map_mount_error(error: Errno) -> RenderproveProtectedMountError {
    match error {
        Errno::NOSYS => RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::MountUnavailable,
            "mount",
            "descriptor-bound Renderprove mount operations are unavailable on this kernel",
        ),
        Errno::ACCESS | Errno::PERM => RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::PermissionDenied,
            "mount",
            "current authority cannot create descriptor-bound Renderprove mounts",
        ),
        Errno::BUSY => RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::MountUnavailable,
            "mount",
            "Renderprove protected mount target is busy",
        ),
        _ => RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "mount",
            "descriptor-bound Renderprove mount operation failed",
        ),
    }
}

fn detach_mount(path: &Path) -> Result<(), RenderproveProtectedMountError> {
    mount::unmount(path, UnmountFlags::DETACH | UnmountFlags::NOFOLLOW).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::CleanupFailed,
            "cleanup",
            "Renderprove protected mount could not be detached",
        )
    })
}

fn remove_alias_directory(
    mount_root: &OwnedFd,
    name: &OsStr,
    expected_base: &OwnedFd,
) -> Result<(), RenderproveProtectedMountError> {
    let reopened =
        fs::openat(mount_root.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
            RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::CleanupFailed,
                "cleanup",
                "Renderprove mount alias could not be verified for cleanup",
            )
        })?;
    if inspect_directory_identity(&reopened, "cleanup")?
        != inspect_directory_identity(expected_base, "cleanup")?
    {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::CleanupFailed,
            "cleanup",
            "Renderprove mount alias changed before cleanup",
        ));
    }
    fs::unlinkat(mount_root.as_fd(), name, AtFlags::REMOVEDIR).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::CleanupFailed,
            "cleanup",
            "Renderprove mount alias could not be removed",
        )
    })
}

fn open_alias_mount(
    mount_root: &OwnedFd,
    name: &OsStr,
    expected: &ReviewedFilesystemIdentity,
    stage: &'static str,
) -> Result<OwnedFd, RenderproveProtectedMountError> {
    let alias =
        fs::openat(mount_root.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
            RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::Io,
                stage,
                "attached Renderprove mount alias could not be opened",
            )
        })?;
    if &inspect_directory_identity(&alias, stage)? != expected {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::IdentityMismatch,
            stage,
            "attached Renderprove mount alias does not match its exact source identity",
        ));
    }
    Ok(alias)
}

fn open_absolute_directory(
    path: &Path,
    stage: &'static str,
) -> Result<OwnedFd, RenderproveProtectedMountError> {
    let components = absolute_components(path)?;
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            stage,
            "filesystem root could not be opened for descriptor traversal",
        )
    })?;
    for component in components {
        current = fs::openat(current.as_fd(), component, DIRECTORY_FLAGS, Mode::empty()).map_err(
            |error| match error {
                Errno::LOOP | Errno::NOTDIR => RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                    stage,
                    "reviewed directory path contains an alias or non-directory component",
                ),
                _ => RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    stage,
                    "reviewed directory path could not be opened safely",
                ),
            },
        )?;
    }
    inspect_directory_identity(&current, stage)?;
    Ok(current)
}

struct CreatedEvidenceDirectory {
    parent: OwnedFd,
    name: OsString,
    directory: OwnedFd,
}

struct PreparedEvidenceDirectory {
    directory: Option<OwnedFd>,
    created: Vec<CreatedEvidenceDirectory>,
    committed: bool,
}

impl PreparedEvidenceDirectory {
    fn directory(&self) -> &OwnedFd {
        self.directory
            .as_ref()
            .expect("prepared evidence directory is retained")
    }

    fn commit(mut self) -> OwnedFd {
        self.committed = true;
        self.directory
            .take()
            .expect("prepared evidence directory is retained")
    }
}

impl Drop for PreparedEvidenceDirectory {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for created in self.created.iter().rev() {
            let _ = remove_created_evidence_directory(created);
        }
    }
}

struct PendingEvidenceDirectory<'a> {
    parent: &'a OwnedFd,
    name: &'a OsStr,
    armed: bool,
}

impl<'a> PendingEvidenceDirectory<'a> {
    fn new(parent: &'a OwnedFd, name: &'a OsStr) -> Self {
        Self {
            parent,
            name,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingEvidenceDirectory<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.parent.as_fd(), self.name, AtFlags::REMOVEDIR);
        }
    }
}

fn prepare_relative_directory(
    project: &OwnedFd,
    path: &Path,
) -> Result<PreparedEvidenceDirectory, RenderproveProtectedMountError> {
    let components = relative_components(path)?;
    let project_stat = fs::fstat(project).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "evidence",
            "reviewed project owner could not be inspected",
        )
    })?;
    if project_stat.st_uid == u32::MAX || project_stat.st_gid == u32::MAX {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "reviewed project owner is invalid for evidence creation",
        ));
    }
    let owner = (project_stat.st_uid, project_stat.st_gid);
    let current = io::dup(project).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "evidence",
            "reviewed project descriptor could not be retained for evidence traversal",
        )
    })?;
    let mut prepared = PreparedEvidenceDirectory {
        directory: Some(current),
        created: Vec::new(),
        committed: false,
    };

    for component in components {
        let current = prepared
            .directory
            .take()
            .expect("prepared evidence directory is retained");
        let next = match fs::openat(current.as_fd(), component, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(directory) => {
                require_evidence_directory_policy(&directory, owner)?;
                directory
            }
            Err(Errno::NOENT) => {
                fs::mkdirat(current.as_fd(), component, PRIVATE_DIRECTORY_MODE).map_err(
                    |error| match error {
                        Errno::EXIST => RenderproveProtectedMountError::new(
                            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                            "evidence",
                            "evidence path changed during descriptor-relative creation",
                        ),
                        Errno::ACCESS | Errno::PERM => RenderproveProtectedMountError::new(
                            RenderproveProtectedMountErrorKind::PermissionDenied,
                            "evidence",
                            "evidence directory could not be created with current authority",
                        ),
                        _ => RenderproveProtectedMountError::new(
                            RenderproveProtectedMountErrorKind::Io,
                            "evidence",
                            "evidence directory could not be created",
                        ),
                    },
                )?;
                let mut pending = PendingEvidenceDirectory::new(&current, component);
                let directory = fs::openat(
                    current.as_fd(),
                    component,
                    MUTABLE_DIRECTORY_FLAGS,
                    Mode::empty(),
                )
                .map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                        "evidence",
                        "created evidence directory could not be retained safely",
                    )
                })?;
                let created_stat = fs::fstat(&directory).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "evidence",
                        "created evidence directory owner could not be inspected",
                    )
                })?;
                if (created_stat.st_uid, created_stat.st_gid) != owner {
                    fs::fchown(
                        &directory,
                        Some(Uid::from_raw(owner.0)),
                        Some(Gid::from_raw(owner.1)),
                    )
                    .map_err(|_| RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::PermissionDenied,
                        "evidence",
                        "created evidence directory ownership could not be bound to the reviewed project",
                    ))?;
                }
                fs::fchmod(&directory, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::PermissionDenied,
                        "evidence",
                        "created evidence directory permissions could not be fixed",
                    )
                })?;
                require_evidence_directory_policy(&directory, owner)?;
                let retained = io::dup(&directory).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "evidence",
                        "created evidence directory identity could not be retained for rollback",
                    )
                })?;
                pending.disarm();
                drop(pending);
                prepared.created.push(CreatedEvidenceDirectory {
                    parent: current,
                    name: component.to_os_string(),
                    directory: retained,
                });
                directory
            }
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                    "evidence",
                    "evidence path contains an alias or non-directory component",
                ));
            }
            Err(_) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    "evidence",
                    "evidence directory could not be opened safely",
                ));
            }
        };
        prepared.directory = Some(next);
    }
    Ok(prepared)
}

fn require_evidence_directory_policy(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), RenderproveProtectedMountError> {
    let stat = fs::fstat(directory).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "evidence",
            "evidence directory identity could not be inspected",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o700 != 0o700
        || stat.st_mode & 0o022 != 0
    {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "evidence directory ownership or mode is unsafe",
        ));
    }
    Ok(())
}

fn remove_created_evidence_directory(
    created: &CreatedEvidenceDirectory,
) -> Result<(), RenderproveProtectedMountError> {
    let reopened = match fs::openat(
        created.parent.as_fd(),
        &created.name,
        DIRECTORY_FLAGS,
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(()),
        Err(_) => {
            return Err(RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::CleanupFailed,
                "evidence_rollback",
                "created evidence directory could not be verified for rollback",
            ));
        }
    };
    if inspect_directory_identity(&reopened, "evidence_rollback")?
        != inspect_directory_identity(&created.directory, "evidence_rollback")?
    {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::CleanupFailed,
            "evidence_rollback",
            "created evidence directory changed before rollback",
        ));
    }
    fs::unlinkat(created.parent.as_fd(), &created.name, AtFlags::REMOVEDIR).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::CleanupFailed,
            "evidence_rollback",
            "created evidence directory could not be rolled back",
        )
    })
}

fn require_root_owned_mount_root(root: &OwnedFd) -> Result<(), RenderproveProtectedMountError> {
    let stat = fs::fstat(root).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "mount_root",
            "Renderprove mount root identity could not be inspected",
        )
    })?;
    if !mount_root_metadata_is_safe(stat.st_uid, stat.st_gid, stat.st_mode) {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "mount_root",
            "Renderprove protected mount root ownership or mode is unsafe",
        ));
    }
    Ok(())
}

fn mount_root_metadata_is_safe(owner_uid: u32, owner_gid: u32, raw_mode: u32) -> bool {
    let permissions = raw_mode & 0o777;
    owner_uid == 0
        && owner_gid == 0
        && permissions & 0o700 == 0o700
        && permissions & 0o066 == 0
        && permissions & 0o001 == 0o001
}

fn inspect_directory_identity(
    descriptor: &impl AsFd,
    stage: &'static str,
) -> Result<ReviewedFilesystemIdentity, RenderproveProtectedMountError> {
    let stat = fs::fstat(descriptor).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            stage,
            "held directory identity could not be inspected",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            stage,
            "held filesystem object is not a directory",
        ));
    }
    ReviewedFilesystemIdentity::new(
        stat.st_dev,
        stat.st_ino,
        stat.st_uid,
        stat.st_gid,
        stat.st_mode & 0o7777,
    )
    .map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            stage,
            "held directory has an invalid exact filesystem identity",
        )
    })
}

fn validate_absolute_path(path: PathBuf) -> Result<PathBuf, RenderproveProtectedMountError> {
    let valid = path.to_str().is_some_and(|value| {
        value.starts_with('/')
            && value != "/"
            && !value.ends_with('/')
            && value[1..]
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..")
    });
    if !valid {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::InvalidSource,
            "source",
            "reviewed Renderprove project path must be absolute, normalized, non-root, and UTF-8",
        ));
    }
    Ok(path)
}

fn absolute_components(path: &Path) -> Result<Vec<&OsStr>, RenderproveProtectedMountError> {
    validate_absolute_path(path.to_path_buf())?;
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect())
}

fn relative_components(path: &Path) -> Result<Vec<&OsStr>, RenderproveProtectedMountError> {
    let valid = path.to_str().is_some_and(|value| {
        !value.is_empty()
            && !value.starts_with('/')
            && !value.ends_with('/')
            && value
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..")
    });
    if !valid {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::InvalidSource,
            "evidence",
            "Renderprove evidence directory must be one normalized project-relative path",
        ));
    }
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::fs::{Mode, OFlags};

    use super::*;
    use crate::artifact::{CommitId, RepositoryRef};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-renderprove-mount-{label}-{}-{sequence}",
                std::process::id()
            ));
            std_fs::create_dir(&path).expect("create temporary directory");
            Self(path)
        }

        fn open(&self) -> OwnedFd {
            fs::open(
                &self.0,
                OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .expect("open temporary directory")
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std_fs::remove_dir_all(&self.0);
        }
    }

    fn source_identity() -> RenderproveSourceIdentity {
        RenderproveSourceIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
        )
    }

    #[derive(Default)]
    struct FakeMountOperations {
        events: Vec<&'static str>,
        attach_calls: usize,
        fail_second_attach: bool,
    }

    impl MountOperations for FakeMountOperations {
        type DetachedMount = u8;

        fn clone_mount(
            &mut self,
            _source: &OwnedFd,
        ) -> Result<Self::DetachedMount, RenderproveProtectedMountError> {
            self.events.push("clone");
            Ok(self.events.len() as u8)
        }

        fn attach_mount(
            &mut self,
            _detached: &Self::DetachedMount,
            _mount_root: &OwnedFd,
            _alias: &OsStr,
        ) -> Result<(), RenderproveProtectedMountError> {
            self.attach_calls += 1;
            self.events.push("attach");
            if self.fail_second_attach && self.attach_calls == 2 {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    "mount",
                    "injected attach failure",
                ));
            }
            Ok(())
        }

        fn detach_mount(
            &mut self,
            _alias_path: &Path,
        ) -> Result<(), RenderproveProtectedMountError> {
            self.events.push("detach");
            Ok(())
        }
    }

    #[test]
    fn source_paths_are_absolute_normalized_and_private() {
        let identity = ReviewedFilesystemIdentity::new(1, 2, 1000, 1000, 0o700).expect("identity");
        for invalid in [
            "relative",
            "/",
            "//tmp/project",
            "/tmp/project/",
            "/tmp//project",
            "/tmp/./project",
            "/tmp/../project",
        ] {
            assert!(
                RenderproveProtectedMountSource::new(source_identity(), invalid, identity.clone(),)
                    .is_err()
            );
        }
        let source = RenderproveProtectedMountSource::new(
            source_identity(),
            "/tmp/private-project",
            identity,
        )
        .expect("source");
        let debug = format!("{source:?}");
        assert!(!debug.contains("/tmp/private-project"));
    }

    #[test]
    fn attached_pair_rolls_back_project_when_evidence_attach_fails() {
        let root = TempRoot::new("rollback");
        let descriptor = root.open();
        let mut backend = FakeMountOperations {
            fail_second_attach: true,
            ..FakeMountOperations::default()
        };
        let error = attach_mount_pair(
            &mut backend,
            &descriptor,
            &descriptor,
            &descriptor,
            OsStr::new("project-00000000000000000000000000000000"),
            OsStr::new("evidence-00000000000000000000000000000000"),
            Path::new("/private/project-alias"),
        )
        .expect_err("second attach fails");
        assert_eq!(error.stage(), "mount");
        assert_eq!(
            backend.events,
            ["clone", "attach", "clone", "attach", "detach"]
        );
    }

    #[test]
    fn attached_pair_has_no_rollback_after_complete_acquisition() {
        let root = TempRoot::new("success");
        let descriptor = root.open();
        let mut backend = FakeMountOperations::default();
        attach_mount_pair(
            &mut backend,
            &descriptor,
            &descriptor,
            &descriptor,
            OsStr::new("project-00000000000000000000000000000000"),
            OsStr::new("evidence-00000000000000000000000000000000"),
            Path::new("/private/project-alias"),
        )
        .expect("attach pair");
        assert_eq!(backend.events, ["clone", "attach", "clone", "attach"]);
    }

    #[test]
    fn alias_collision_retries_without_using_caller_selected_names() {
        let root = TempRoot::new("aliases");
        let root_fd = root.open();
        let collision = OsString::from("project-00000000000000000000000000000000");
        fs::mkdirat(root_fd.as_fd(), &collision, PRIVATE_DIRECTORY_MODE).expect("create collision");
        let success = OsString::from("project-11111111111111111111111111111111");
        let mut candidates = vec![collision, success.clone()].into_iter();
        let alias = create_alias_directory_with_generator(&root_fd, "project", || {
            Ok(candidates.next().expect("candidate"))
        })
        .expect("retry collision");
        assert_eq!(alias.name, success);
        remove_alias_directory(&root_fd, &alias.name, &alias.base).expect("remove alias");
    }

    #[test]
    fn prepared_evidence_is_private_owned_and_rolls_back_until_commit() {
        let root = TempRoot::new("evidence-rollback");
        let project = root.open();
        let project_stat = fs::fstat(&project).expect("project stat");
        {
            let prepared = prepare_relative_directory(&project, Path::new("artifacts/renderprove"))
                .expect("prepare evidence");
            let evidence_stat = fs::fstat(prepared.directory()).expect("evidence stat");
            assert_eq!(evidence_stat.st_uid, project_stat.st_uid);
            assert_eq!(evidence_stat.st_gid, project_stat.st_gid);
            assert_eq!(evidence_stat.st_mode & 0o7777, 0o700);
        }
        assert!(!root.0.join("artifacts").exists());
    }

    #[test]
    fn pending_alias_removes_created_directory_on_failure() {
        let root = TempRoot::new("alias-rollback");
        let root_fd = root.open();
        let name = OsString::from("project-22222222222222222222222222222222");
        fs::mkdirat(root_fd.as_fd(), &name, PRIVATE_DIRECTORY_MODE).expect("create alias");
        {
            let _pending = PendingAlias::new(&root_fd, &name);
        }
        assert!(!root.0.join(&name).exists());
    }

    #[test]
    fn mount_root_is_private_but_runner_traversable() {
        assert!(mount_root_metadata_is_safe(0, 0, 0o040711));
        assert!(!mount_root_metadata_is_safe(0, 0, 0o040700));
        assert!(!mount_root_metadata_is_safe(0, 0, 0o040755));
        assert!(!mount_root_metadata_is_safe(0, 0, 0o040733));
        assert!(!mount_root_metadata_is_safe(1000, 0, 0o040711));
    }

    #[test]
    fn errors_and_cleanup_receipts_are_bounded() {
        let error = RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "mount",
            "bounded mount failure",
        );
        let encoded = serde_json::to_string(&error).expect("serialize error");
        assert!(!encoded.contains("/run/smolrunner"));
        assert!(!encoded.contains("inode"));
        let receipt = RenderproveProtectedMountCleanupReceipt {
            schema_version: RENDERPROVE_PROTECTED_MOUNT_LEASE_SCHEMA_VERSION,
            project_detached: true,
            evidence_detached: true,
            aliases_removed: true,
        };
        assert!(receipt.project_detached());
        assert!(receipt.evidence_detached());
        assert!(receipt.aliases_removed());
    }
}
