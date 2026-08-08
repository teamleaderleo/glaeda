use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use rustix::process::{getegid, geteuid};
use serde::Serialize;

use crate::operator_config::{
    MAX_OPERATOR_CONFIG_DOCUMENT_BYTES, MAX_OPERATOR_CONFIG_PATH_BYTES, OperatorConfig,
    OperatorConfigPublicSummary,
};
use crate::operator_error::{OperatorErrorCode, OperatorPublicError};

const CONFIG_ENVIRONMENT_KEY: &str = "SMOLRUNNER_CONFIG";
const DEFAULT_LIBRARY_DIRECTORY: &str = "Library";
const DEFAULT_APPLICATION_SUPPORT_DIRECTORY: &str = "Application Support";
const DEFAULT_MANAGED_DIRECTORY: &str = "SmolRunner";
const CONFIG_FILE: &str = "config.json";
const STAGED_CONFIG_FILE: &str = ".config.json.next";
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const NEW_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

#[derive(Clone, Default, PartialEq, Eq)]
pub struct OperatorConfigDiscoveryRequest {
    explicit_path: Option<OsString>,
}

impl fmt::Debug for OperatorConfigDiscoveryRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorConfigDiscoveryRequest")
            .field(
                "explicit_path",
                &self
                    .explicit_path
                    .as_ref()
                    .map(|_| "<private-operator-config-path>"),
            )
            .finish()
    }
}

impl OperatorConfigDiscoveryRequest {
    #[must_use]
    pub fn new(explicit_path: Option<OsString>) -> Self {
        Self { explicit_path }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorConfigSource {
    Explicit,
    Environment,
    MacosDefault,
}

#[derive(Clone, PartialEq, Eq)]
struct ResolvedOperatorConfigLocation {
    path: PathBuf,
    source: OperatorConfigSource,
}

impl fmt::Debug for ResolvedOperatorConfigLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedOperatorConfigLocation")
            .field("path", &"<private-operator-config-path>")
            .field("source", &self.source)
            .finish()
    }
}

pub trait OperatorConfigDiscoveryContext {
    fn environment_config(&self) -> Option<OsString>;
    fn operator_home(&self) -> Option<OsString>;
    fn supports_macos_default(&self) -> bool;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemOperatorConfigDiscoveryContext;

impl OperatorConfigDiscoveryContext for SystemOperatorConfigDiscoveryContext {
    fn environment_config(&self) -> Option<OsString> {
        std::env::var_os(CONFIG_ENVIRONMENT_KEY)
    }

    fn operator_home(&self) -> Option<OsString> {
        std::env::var_os("HOME")
    }

    fn supports_macos_default(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct OperatorConfigLoad {
    source: OperatorConfigSource,
    config: OperatorConfig,
}

impl OperatorConfigLoad {
    #[must_use]
    pub const fn source(&self) -> OperatorConfigSource {
        self.source
    }

    #[must_use]
    pub const fn config(&self) -> &OperatorConfig {
        &self.config
    }
}

impl fmt::Debug for OperatorConfigLoad {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorConfigLoad")
            .field("source", &self.source)
            .field("config", &self.config.public_summary())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorConfigCreateDisposition {
    Created,
    AlreadyExists,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorConfigCreateReceipt {
    disposition: OperatorConfigCreateDisposition,
    source: OperatorConfigSource,
    config: OperatorConfigPublicSummary,
    bytes_written: usize,
}

impl OperatorConfigCreateReceipt {
    #[must_use]
    pub const fn disposition(&self) -> OperatorConfigCreateDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn source(&self) -> OperatorConfigSource {
        self.source
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorConfigStoreErrorKind {
    Missing,
    InvalidLocation,
    UnsafeFilesystem,
    InvalidDocument,
    UnsupportedVersion,
    Incompatible,
    Io,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorConfigStoreError {
    kind: OperatorConfigStoreErrorKind,
    public: OperatorPublicError,
    message: &'static str,
}

impl OperatorConfigStoreError {
    const fn new(
        kind: OperatorConfigStoreErrorKind,
        code: OperatorErrorCode,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            public: OperatorPublicError::from_code(code),
            message,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> OperatorConfigStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn public(&self) -> &OperatorPublicError {
        &self.public
    }
}

impl fmt::Display for OperatorConfigStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for OperatorConfigStoreError {}

#[derive(Debug, Clone, Copy, Default)]
pub struct OperatorConfigStore;

impl OperatorConfigStore {
    pub fn load<C: OperatorConfigDiscoveryContext>(
        request: &OperatorConfigDiscoveryRequest,
        context: &C,
    ) -> Result<OperatorConfigLoad, OperatorConfigStoreError> {
        let location = resolve_location(request, context)?;
        let config = load_at(&location)?;
        Ok(OperatorConfigLoad {
            source: location.source,
            config,
        })
    }

    pub fn create<C: OperatorConfigDiscoveryContext>(
        request: &OperatorConfigDiscoveryRequest,
        context: &C,
        config: &OperatorConfig,
    ) -> Result<OperatorConfigCreateReceipt, OperatorConfigStoreError> {
        let location = resolve_location(request, context)?;
        match load_at(&location) {
            Ok(_) => {
                let existing = load_durable_existing(&location)?;
                return existing_receipt(&location, &existing, config);
            }
            Err(error) if error.kind() != OperatorConfigStoreErrorKind::Missing => {
                return Err(error);
            }
            Err(_) => {}
        }
        create_at(&location, config)
    }
}

fn resolve_location<C: OperatorConfigDiscoveryContext>(
    request: &OperatorConfigDiscoveryRequest,
    context: &C,
) -> Result<ResolvedOperatorConfigLocation, OperatorConfigStoreError> {
    if let Some(path) = &request.explicit_path {
        return Ok(ResolvedOperatorConfigLocation {
            path: validate_config_path(path)?,
            source: OperatorConfigSource::Explicit,
        });
    }
    if let Some(path) = context.environment_config() {
        return Ok(ResolvedOperatorConfigLocation {
            path: validate_config_path(&path)?,
            source: OperatorConfigSource::Environment,
        });
    }
    if !context.supports_macos_default() {
        return Err(OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::UnsupportedPlatform,
            OperatorErrorCode::UnsupportedPlatform,
            "the reviewed operator configuration default is unavailable on this platform",
        ));
    }
    let home = validate_config_path(&context.operator_home().ok_or_else(|| {
        OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::InvalidLocation,
            OperatorErrorCode::ConfigurationInvalid,
            "the operator home directory is unavailable",
        )
    })?)?;
    let path = home
        .join(DEFAULT_LIBRARY_DIRECTORY)
        .join(DEFAULT_APPLICATION_SUPPORT_DIRECTORY)
        .join(DEFAULT_MANAGED_DIRECTORY)
        .join(CONFIG_FILE);
    Ok(ResolvedOperatorConfigLocation {
        path: validate_config_path(path.as_os_str())?,
        source: OperatorConfigSource::MacosDefault,
    })
}

fn validate_config_path(value: &OsStr) -> Result<PathBuf, OperatorConfigStoreError> {
    let bytes = value.as_bytes();
    let path = Path::new(value);
    let valid_components = path
        .components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)));
    if bytes.is_empty()
        || bytes.len() > MAX_OPERATOR_CONFIG_PATH_BYTES
        || bytes == b"/"
        || !path.is_absolute()
        || !valid_components
        || bytes.ends_with(b"/")
        || bytes.windows(2).any(|pair| pair == b"//")
        || bytes.iter().any(|byte| byte.is_ascii_control())
        || value.to_str().is_none()
    {
        return Err(OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::InvalidLocation,
            OperatorErrorCode::ConfigurationInvalid,
            "the selected operator configuration location is invalid",
        ));
    }
    Ok(path.to_path_buf())
}

fn load_at(
    location: &ResolvedOperatorConfigLocation,
) -> Result<OperatorConfig, OperatorConfigStoreError> {
    let exact_private_parent = location.source == OperatorConfigSource::MacosDefault;
    let (parent, file_name) = open_parent(&location.path, false, exact_private_parent, false)?;
    let parent_stat = fs::fstat(&parent)
        .map_err(|_| io_error("could not inspect the configuration directory"))?;
    refuse_staged_config(&parent)?;
    let file = fs::openat(&parent, file_name, FILE_FLAGS, Mode::empty()).map_err(map_file_open)?;
    let before = inspect_private_file(&file, None)?;
    let mut bytes = Vec::new();
    let mut file = File::from(file);
    std::io::Read::by_ref(&mut file)
        .take((MAX_OPERATOR_CONFIG_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| io_error("could not read the operator configuration"))?;
    if bytes.len() > MAX_OPERATOR_CONFIG_DOCUMENT_BYTES {
        return Err(invalid_document("the operator configuration is oversized"));
    }
    let after = inspect_private_file(&file, Some(bytes.len()))?;
    if !stable_stat(&before, &after) {
        return Err(unsafe_filesystem(
            "the operator configuration changed while it was read",
        ));
    }
    let (current_parent, current_name) =
        open_parent(&location.path, false, exact_private_parent, false)?;
    let current_parent_stat = fs::fstat(&current_parent)
        .map_err(|_| io_error("could not re-inspect the configuration directory"))?;
    if !same_object(&parent_stat, &current_parent_stat) {
        return Err(unsafe_filesystem(
            "the operator configuration directory changed while it was read",
        ));
    }
    refuse_staged_config(&current_parent)?;
    let current = fs::openat(&current_parent, current_name, FILE_FLAGS, Mode::empty())
        .map_err(map_file_open)?;
    let current_stat = inspect_private_file(&current, Some(bytes.len()))?;
    if !same_object(&after, &current_stat) {
        return Err(unsafe_filesystem(
            "the operator configuration name changed while it was read",
        ));
    }
    OperatorConfig::decode_persisted_json(&bytes).map_err(|error| {
        if error.code == "unsupported_schema_version" {
            OperatorConfigStoreError::new(
                OperatorConfigStoreErrorKind::UnsupportedVersion,
                OperatorErrorCode::ConfigurationVersionUnsupported,
                "the operator configuration version is unsupported",
            )
        } else {
            invalid_document("the operator configuration document is invalid")
        }
    })
}

fn refuse_staged_config(parent: &OwnedFd) -> Result<(), OperatorConfigStoreError> {
    match fs::openat(parent, STAGED_CONFIG_FILE, FILE_FLAGS, Mode::empty()) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) => Err(invalid_document(
            "staged operator configuration evidence requires repair",
        )),
        Err(Errno::LOOP | Errno::NOTDIR | Errno::ISDIR) => Err(unsafe_filesystem(
            "the staged operator configuration path is unsafe",
        )),
        Err(_) => Err(io_error(
            "could not inspect staged operator configuration evidence",
        )),
    }
}

fn create_at(
    location: &ResolvedOperatorConfigLocation,
    config: &OperatorConfig,
) -> Result<OperatorConfigCreateReceipt, OperatorConfigStoreError> {
    let bytes = config
        .encode_persisted_json()
        .map_err(|_| invalid_document("the operator configuration could not be encoded"))?;
    let (parent, file_name) = open_parent(
        &location.path,
        location.source == OperatorConfigSource::MacosDefault,
        location.source == OperatorConfigSource::MacosDefault,
        location.source == OperatorConfigSource::MacosDefault,
    )?;
    let parent_identity = fs::fstat(&parent)
        .map_err(|_| io_error("could not inspect the configuration directory"))?;
    let staged = fs::openat(
        &parent,
        STAGED_CONFIG_FILE,
        NEW_FILE_FLAGS,
        PRIVATE_FILE_MODE,
    )
    .map_err(map_stage_create)?;
    let staged_identity = fs::fstat(&staged)
        .map_err(|_| io_error("could not inspect the staged operator configuration"))?;
    let (device, inode) = stat_identity(&staged_identity).ok_or_else(|| {
        unsafe_filesystem("the staged operator configuration identity is invalid")
    })?;
    let mut guard = StagedConfig {
        parent: parent.as_fd(),
        device,
        inode,
        armed: true,
    };
    fs::fchmod(&staged, PRIVATE_FILE_MODE)
        .map_err(|_| io_error("could not set private configuration permissions"))?;
    inspect_private_file(&staged, Some(0))?;
    let mut staged = File::from(staged);
    staged
        .write_all(&bytes)
        .map_err(|_| io_error("could not write the staged operator configuration"))?;
    staged
        .sync_all()
        .map_err(|_| io_error("could not synchronize the staged operator configuration"))?;
    inspect_private_file(&staged, Some(bytes.len()))?;
    match fs::renameat_with(
        &parent,
        STAGED_CONFIG_FILE,
        &parent,
        file_name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            fs::fsync(&parent)
                .map_err(|_| io_error("could not synchronize the configuration directory"))?;
            verify_published_config(
                location,
                &parent_identity,
                &staged_identity,
                bytes.as_slice(),
            )?;
            guard.armed = false;
            Ok(OperatorConfigCreateReceipt {
                disposition: OperatorConfigCreateDisposition::Created,
                source: location.source,
                config: config.public_summary(),
                bytes_written: bytes.len(),
            })
        }
        Err(Errno::EXIST) => {
            drop(staged);
            drop(guard);
            let existing = load_durable_existing(location)?;
            existing_receipt(location, &existing, config)
        }
        Err(_) => Err(io_error(
            "could not publish the operator configuration atomically",
        )),
    }
}

fn load_durable_existing(
    location: &ResolvedOperatorConfigLocation,
) -> Result<OperatorConfig, OperatorConfigStoreError> {
    synchronize_config_parent(location)?;
    load_at(location)
}

fn verify_published_config(
    location: &ResolvedOperatorConfigLocation,
    expected_parent: &rustix::fs::Stat,
    expected_file: &rustix::fs::Stat,
    expected_bytes: &[u8],
) -> Result<(), OperatorConfigStoreError> {
    let (parent, file_name) = open_parent(
        &location.path,
        false,
        location.source == OperatorConfigSource::MacosDefault,
        false,
    )?;
    let parent_stat = fs::fstat(&parent)
        .map_err(|_| io_error("could not re-inspect the configuration directory"))?;
    if !same_object(expected_parent, &parent_stat) {
        return Err(unsafe_filesystem(
            "the configuration directory changed during publication",
        ));
    }
    refuse_staged_config(&parent)?;
    let file = fs::openat(&parent, file_name, FILE_FLAGS, Mode::empty()).map_err(map_file_open)?;
    let before = inspect_private_file(&file, Some(expected_bytes.len()))?;
    if !same_object(expected_file, &before) {
        return Err(unsafe_filesystem(
            "the operator configuration name changed during publication",
        ));
    }
    let mut observed_bytes = Vec::new();
    let mut file = File::from(file);
    std::io::Read::by_ref(&mut file)
        .take((MAX_OPERATOR_CONFIG_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut observed_bytes)
        .map_err(|_| io_error("could not verify the published operator configuration"))?;
    let after = inspect_private_file(&file, Some(expected_bytes.len()))?;
    if !stable_stat(&before, &after) || observed_bytes != expected_bytes {
        return Err(unsafe_filesystem(
            "the operator configuration changed during publication",
        ));
    }
    Ok(())
}

fn synchronize_config_parent(
    location: &ResolvedOperatorConfigLocation,
) -> Result<(), OperatorConfigStoreError> {
    let (parent, _) = open_parent(
        &location.path,
        false,
        location.source == OperatorConfigSource::MacosDefault,
        location.source == OperatorConfigSource::MacosDefault,
    )?;
    fs::fsync(&parent).map_err(|_| io_error("could not synchronize the configuration directory"))
}

fn existing_receipt(
    location: &ResolvedOperatorConfigLocation,
    existing: &OperatorConfig,
    requested: &OperatorConfig,
) -> Result<OperatorConfigCreateReceipt, OperatorConfigStoreError> {
    if existing.identity() != requested.identity() {
        return Err(OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::Incompatible,
            OperatorErrorCode::ConfigurationIncompatible,
            "an incompatible operator configuration already exists",
        ));
    }
    Ok(OperatorConfigCreateReceipt {
        disposition: OperatorConfigCreateDisposition::AlreadyExists,
        source: location.source,
        config: existing.public_summary(),
        bytes_written: 0,
    })
}

fn open_parent(
    path: &Path,
    create_default_parent: bool,
    exact_private_parent: bool,
    synchronize_final_parent_entry: bool,
) -> Result<(OwnedFd, &OsStr), OperatorConfigStoreError> {
    let file_name = path.file_name().ok_or_else(|| {
        OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::InvalidLocation,
            OperatorErrorCode::ConfigurationInvalid,
            "the selected operator configuration location is invalid",
        )
    })?;
    let components: Vec<_> = path
        .parent()
        .expect("validated path has a parent")
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| io_error("could not open the filesystem root"))?;
    for (index, component) in components.iter().enumerate() {
        let final_parent = index + 1 == components.len();
        let mut opened = match fs::openat(&current, *component, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(directory) => directory,
            Err(Errno::NOENT) if final_parent && create_default_parent => {
                let created = match fs::mkdirat(&current, *component, PRIVATE_DIRECTORY_MODE) {
                    Ok(()) => true,
                    Err(Errno::EXIST) => false,
                    Err(_) => {
                        return Err(io_error(
                            "could not create the managed configuration directory",
                        ));
                    }
                };
                let directory = fs::openat(&current, *component, DIRECTORY_FLAGS, Mode::empty())
                    .map_err(map_directory_open)?;
                if created {
                    fs::fchmod(&directory, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                        io_error("could not set private managed-directory permissions")
                    })?;
                }
                directory
            }
            Err(error) => return Err(map_directory_open(error)),
        };
        let opened_stat =
            inspect_directory(&opened, final_parent, exact_private_parent && final_parent)?;
        if final_parent && synchronize_final_parent_entry {
            fs::fsync(&current)
                .map_err(|_| io_error("could not synchronize the configuration parent"))?;
            let rebound = fs::openat(&current, *component, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_directory_open)?;
            let rebound_stat =
                inspect_directory(&rebound, true, exact_private_parent && final_parent)?;
            if !same_object(&opened_stat, &rebound_stat) {
                return Err(unsafe_filesystem(
                    "the managed configuration directory changed during publication",
                ));
            }
            opened = rebound;
        }
        current = opened;
    }
    Ok((current, file_name))
}

fn inspect_directory(
    directory: impl AsFd,
    final_parent: bool,
    exact_private_mode: bool,
) -> Result<rustix::fs::Stat, OperatorConfigStoreError> {
    let stat = fs::fstat(directory.as_fd())
        .map_err(|_| io_error("could not inspect a configuration directory"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_mode & 0o022 != 0
        || (stat.st_uid != 0 && stat.st_uid != geteuid().as_raw())
        || (final_parent && stat.st_uid != geteuid().as_raw())
        || (exact_private_mode && stat.st_mode & 0o7777 != 0o700)
    {
        return Err(unsafe_filesystem(
            "an operator configuration directory is unsafe",
        ));
    }
    Ok(stat)
}

fn inspect_private_file(
    file: impl AsFd,
    expected_size: Option<usize>,
) -> Result<rustix::fs::Stat, OperatorConfigStoreError> {
    let stat = fs::fstat(file.as_fd())
        .map_err(|_| io_error("could not inspect the operator configuration"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_uid != geteuid().as_raw()
        || stat.st_gid != getegid().as_raw()
    {
        return Err(unsafe_filesystem(
            "the operator configuration object is unsafe",
        ));
    }
    if expected_size.is_some_and(|expected| {
        stat.st_size < 0 || u64::try_from(expected).ok() != Some(stat.st_size as u64)
    }) {
        return Err(invalid_document(
            "the operator configuration size is inconsistent",
        ));
    }
    Ok(stat)
}

fn stable_stat(before: &rustix::fs::Stat, after: &rustix::fs::Stat) -> bool {
    same_object(before, after)
        && before.st_mode == after.st_mode
        && before.st_nlink == after.st_nlink
        && before.st_uid == after.st_uid
        && before.st_gid == after.st_gid
        && before.st_size == after.st_size
        && before.st_mtime == after.st_mtime
        && before.st_mtime_nsec == after.st_mtime_nsec
        && before.st_ctime == after.st_ctime
        && before.st_ctime_nsec == after.st_ctime_nsec
}

fn same_object(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn stat_identity(stat: &rustix::fs::Stat) -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    let device = u64::try_from(stat.st_dev).ok()?;
    #[cfg(not(target_os = "macos"))]
    let device = stat.st_dev;
    Some((device, stat.st_ino))
}

struct StagedConfig<'a> {
    parent: BorrowedFd<'a>,
    device: u64,
    inode: u64,
    armed: bool,
}

impl Drop for StagedConfig<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Ok(current) =
                fs::openat(self.parent, STAGED_CONFIG_FILE, FILE_FLAGS, Mode::empty())
            && fs::fstat(&current)
                .ok()
                .and_then(|stat| stat_identity(&stat))
                == Some((self.device, self.inode))
        {
            let _ = fs::unlinkat(self.parent, STAGED_CONFIG_FILE, AtFlags::empty());
        }
    }
}

fn map_directory_open(error: Errno) -> OperatorConfigStoreError {
    match error {
        Errno::NOENT => OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::Missing,
            OperatorErrorCode::ConfigurationMissing,
            "the selected operator configuration directory is missing",
        ),
        Errno::LOOP | Errno::NOTDIR => {
            unsafe_filesystem("an operator configuration directory is symlinked or invalid")
        }
        _ => io_error("could not open an operator configuration directory"),
    }
}

fn map_file_open(error: Errno) -> OperatorConfigStoreError {
    match error {
        Errno::NOENT => OperatorConfigStoreError::new(
            OperatorConfigStoreErrorKind::Missing,
            OperatorErrorCode::ConfigurationMissing,
            "the selected operator configuration is missing",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => {
            unsafe_filesystem("the selected operator configuration is symlinked or invalid")
        }
        _ => io_error("could not open the selected operator configuration"),
    }
}

fn map_stage_create(error: Errno) -> OperatorConfigStoreError {
    match error {
        Errno::EXIST => invalid_document(
            "staged operator configuration evidence already exists and requires repair",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => {
            unsafe_filesystem("the staged operator configuration path is unsafe")
        }
        _ => io_error("could not create a staged operator configuration"),
    }
}

fn invalid_document(message: &'static str) -> OperatorConfigStoreError {
    OperatorConfigStoreError::new(
        OperatorConfigStoreErrorKind::InvalidDocument,
        OperatorErrorCode::ConfigurationInvalid,
        message,
    )
}

fn unsafe_filesystem(message: &'static str) -> OperatorConfigStoreError {
    OperatorConfigStoreError::new(
        OperatorConfigStoreErrorKind::UnsafeFilesystem,
        OperatorErrorCode::ConfigurationInvalid,
        message,
    )
}

fn io_error(message: &'static str) -> OperatorConfigStoreError {
    OperatorConfigStoreError::new(
        OperatorConfigStoreErrorKind::Io,
        OperatorErrorCode::ConfigurationInvalid,
        message,
    )
}
