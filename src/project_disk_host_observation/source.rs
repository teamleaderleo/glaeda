#[cfg(target_os = "macos")]
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::lima_observation::{LimaInstanceName, LimaObservationRequest};

use super::raw::{
    HeldLimaSource as RawHeldLimaSource, LimaStandaloneDiskName,
    LimaStandaloneDiskObservationRequest as RawObservationRequest, ProjectDiskHostObservationError,
};

const MAX_LIMA_HOME_BYTES: usize = 1_024;
const SOURCE_IDENTITY_DOMAIN: &[u8] = b"smolrunner-project-disk-lima-source-v1";

/// Persistable equality identity for one configured private Lima namespace.
///
/// This identity says that P2/P3 adapters target the same configured namespace. Current physical
/// safety still comes from fresh descriptor-bound observation; this digest never proves that a
/// surviving directory is the same physical object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskLimaSourceIdentity(Sha256Digest);

impl ProjectDiskLimaSourceIdentity {
    /// Parse one persisted canonical source identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless `value` is canonical SHA-256.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskLimaSourceIdentityParseError> {
        Sha256Digest::parse(value)
            .map(Self)
            .map_err(|_| ProjectDiskLimaSourceIdentityParseError)
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskLimaSourceIdentityParseError;

impl ProjectDiskLimaSourceIdentityParseError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        "project_disk_lima_source_identity_invalid"
    }
}

impl fmt::Display for ProjectDiskLimaSourceIdentityParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("project disk Lima source identity must be canonical SHA-256")
    }
}

impl std::error::Error for ProjectDiskLimaSourceIdentityParseError {}

/// Canonical configured Lima namespace used to derive P2 planned requests.
///
/// This value deliberately carries no live physical-source authority. It validates only the
/// configured pathname spelling and the one physically established macOS `/var -> /private/var`
/// compatibility alias. `hold` opens and binds the source descriptor before inventory capture;
/// #699 owns stronger durable physical source identity across restart/replacement.
pub struct ConfiguredProjectDiskLimaSource {
    canonical_lima_home: PathBuf,
    identity: ProjectDiskLimaSourceIdentity,
}

impl ConfiguredProjectDiskLimaSource {
    /// Canonicalize one configured private Lima home and derive its namespace identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-free refusal for a non-absolute/non-canonical source or an invalid
    /// macOS `/var` compatibility alias.
    pub fn new(lima_home: impl Into<PathBuf>) -> Result<Self, ProjectDiskLimaSourceError> {
        let normalized = normalize_source_path(&lima_home.into())?;
        let canonical_lima_home = accepted_source_path(&normalized)?;
        let identity = derive_source_identity(&canonical_lima_home);
        Ok(Self {
            canonical_lima_home,
            identity,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.identity
    }

    /// Open and seal the exact configured source before inventory is captured.
    pub fn hold(&self) -> Result<HeldProjectDiskLimaSource, ProjectDiskHostObservationError> {
        let inner = RawHeldLimaSource::open(self.canonical_lima_home.clone())?;
        Ok(HeldProjectDiskLimaSource {
            identity: self.identity.clone(),
            inner,
        })
    }
}

/// Short-lived, non-cloneable binding to one current physical Lima source.
///
/// This is process-local authority only, is intentionally not serializable, and makes no durable
/// across-restart identity claim.
pub struct HeldProjectDiskLimaSource {
    identity: ProjectDiskLimaSourceIdentity,
    inner: RawHeldLimaSource,
}

impl HeldProjectDiskLimaSource {
    #[must_use]
    pub const fn identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.identity
    }

    /// Reconfirm that the configured path still resolves to this exact held source.
    ///
    /// This exposes no path, descriptor, process hook, or callback. The concrete #696 inventory
    /// operation will remain inside this owning module and bracket its fixed child invocation with
    /// this confirmation while borrowing the otherwise-private held state.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn confirm_path_binding(&self) -> Result<(), ProjectDiskHostObservationError> {
        self.inner.confirm_path_binding()
    }

    pub(crate) fn resident_observation_request(
        &self,
        instance: LimaInstanceName,
        guest_cache_path: &Path,
        max_age_seconds: u64,
    ) -> Result<LimaObservationRequest, ProjectDiskHostObservationError> {
        self.inner
            .resident_observation_request(instance, guest_cache_path, max_age_seconds)
    }

    pub(crate) fn confirm_resident_instance_absent(
        &self,
        instance: &LimaInstanceName,
    ) -> Result<(), ProjectDiskHostObservationError> {
        self.inner.confirm_resident_instance_absent(instance)
    }
}

impl fmt::Debug for HeldProjectDiskLimaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeldProjectDiskLimaSource")
            .field("identity", &self.identity)
            .field("physical_binding", &"<private-source-binding>")
            .finish()
    }
}

impl fmt::Debug for ConfiguredProjectDiskLimaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredProjectDiskLimaSource")
            .field("identity", &self.identity)
            .field("private_lima_home", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskLimaSourceError {
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskLimaSourceError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ProjectDiskLimaSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskLimaSourceError {}

/// Production P2 request derived only from one configured Lima source and controller-selected disk
/// locator. Product callers cannot supply the `_disks/<locator>` directory independently. The
/// resulting observation still performs fresh physical source/collection validation.
pub struct LimaStandaloneDiskObservationRequest {
    inner: RawObservationRequest,
    source_identity: ProjectDiskLimaSourceIdentity,
    disk_name: LimaStandaloneDiskName,
}

impl LimaStandaloneDiskObservationRequest {
    /// Derive the reviewed Lima 2.2.0 standalone-disk path for one planned locator.
    ///
    /// # Errors
    ///
    /// Returns the existing bounded P2 refusal if the derived request cannot satisfy the raw
    /// descriptor engine's exact path contract.
    pub fn for_planned_disk(
        source: HeldProjectDiskLimaSource,
        disk_name: LimaStandaloneDiskName,
    ) -> Result<Self, ProjectDiskHostObservationError> {
        let identity = source.identity.clone();
        let inner = source
            .inner
            .into_planned_request(disk_name.clone(), identity.clone())?;
        Ok(Self {
            inner,
            source_identity: identity,
            disk_name,
        })
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.disk_name
    }

    pub(super) fn into_raw(self) -> RawObservationRequest {
        self.inner
    }
}

impl fmt::Debug for LimaStandaloneDiskObservationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaStandaloneDiskObservationRequest")
            .field("disk_name", &self.disk_name)
            .field("source_identity", &self.source_identity)
            .field("private_paths", &"<redacted>")
            .finish()
    }
}

/// Explicit-path request for the retained unbound physical fixture/diagnostic path.
///
/// This type is intentionally distinct from the production planned request, so #644 cannot accept
/// an operator-supplied directory accidentally. It still returns only unbound P2 observation.
pub struct LimaStandaloneDiskFixtureObservationRequest {
    inner: RawObservationRequest,
}

impl LimaStandaloneDiskFixtureObservationRequest {
    /// Bind one explicitly supplied research/fixture directory to the raw read-only observer.
    ///
    /// # Errors
    ///
    /// Returns the existing bounded P2 refusal for an invalid explicit fixture path.
    pub fn new(
        disk_name: LimaStandaloneDiskName,
        lima_home: impl Into<PathBuf>,
        disk_directory: impl Into<PathBuf>,
    ) -> Result<Self, ProjectDiskHostObservationError> {
        RawObservationRequest::new(disk_name, lima_home, disk_directory).map(|inner| Self { inner })
    }

    pub(super) fn into_raw(self) -> RawObservationRequest {
        self.inner
    }
}

impl fmt::Debug for LimaStandaloneDiskFixtureObservationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaStandaloneDiskFixtureObservationRequest")
            .field("private_paths", &"<redacted>")
            .finish()
    }
}

fn normalize_source_path(path: &Path) -> Result<PathBuf, ProjectDiskLimaSourceError> {
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > MAX_LIMA_HOME_BYTES {
        return Err(invalid_source());
    }

    let mut normalized = PathBuf::from("/");
    let mut saw_normal = false;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => {
                saw_normal = true;
                normalized.push(value);
            }
            _ => return Err(invalid_source()),
        }
    }
    if !saw_normal || normalized.as_os_str().as_bytes().len() > MAX_LIMA_HOME_BYTES {
        return Err(invalid_source());
    }
    Ok(normalized)
}

#[cfg(target_os = "macos")]
fn accepted_source_path(path: &Path) -> Result<PathBuf, ProjectDiskLimaSourceError> {
    use std::os::unix::fs::MetadataExt as _;

    let mut components = path.components();
    let _ = components.next();
    if components.next() != Some(Component::Normal(OsStr::new("var"))) {
        return Ok(path.to_owned());
    }

    let alias = std::fs::symlink_metadata("/var").map_err(|_| unsafe_source())?;
    if !alias.file_type().is_symlink() || alias.uid() != 0 {
        return Err(unsafe_source());
    }
    let target = std::fs::read_link("/var").map_err(|_| unsafe_source())?;
    if target != Path::new("private/var") && target != Path::new("/private/var") {
        return Err(unsafe_source());
    }

    let suffix = path.strip_prefix("/var").map_err(|_| unsafe_source())?;
    Ok(Path::new("/private/var").join(suffix))
}

#[cfg(not(target_os = "macos"))]
fn accepted_source_path(path: &Path) -> Result<PathBuf, ProjectDiskLimaSourceError> {
    Ok(path.to_owned())
}

fn derive_source_identity(path: &Path) -> ProjectDiskLimaSourceIdentity {
    let path_bytes = path.as_os_str().as_bytes();
    let path_len = u64::try_from(path_bytes.len()).expect("bounded Lima source path fits in u64");
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_IDENTITY_DOMAIN);
    hasher.update(path_len.to_be_bytes());
    hasher.update(path_bytes);
    let digest = hasher.finalize();
    let mut canonical = String::with_capacity(71);
    canonical.push_str("sha256:");
    for byte in digest {
        write!(&mut canonical, "{byte:02x}").expect("writing to String cannot fail");
    }
    ProjectDiskLimaSourceIdentity(
        Sha256Digest::parse(&canonical).expect("locally produced SHA-256 is canonical"),
    )
}

const fn invalid_source() -> ProjectDiskLimaSourceError {
    ProjectDiskLimaSourceError {
        code: "project_disk_lima_source_invalid",
        message: "project disk Lima source must be one bounded normalized absolute path",
    }
}

#[cfg(target_os = "macos")]
const fn unsafe_source() -> ProjectDiskLimaSourceError {
    ProjectDiskLimaSourceError {
        code: "project_disk_lima_source_unsafe",
        message: "project disk Lima source uses an unsafe system alias",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_request_derives_reviewed_collection_and_hides_paths() {
        use std::os::unix::fs::PermissionsExt as _;

        let path =
            std::env::temp_dir().join(format!("smolrunner-p2-source-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = ConfiguredProjectDiskLimaSource::new(&path).unwrap();
        let disk_name = LimaStandaloneDiskName::parse("srpd1-test").unwrap();
        let request = LimaStandaloneDiskObservationRequest::for_planned_disk(
            source.hold().unwrap(),
            disk_name,
        )
        .unwrap();

        assert_eq!(request.disk_name().as_str(), "srpd1-test");
        assert_eq!(request.source_identity(), source.identity());
        let debug = format!("{request:?}");
        assert!(!debug.contains(path.to_str().unwrap()));
        assert!(debug.contains("source_identity"));
        std::fs::remove_dir(&path).unwrap();
    }

    #[test]
    fn source_identity_is_path_private_and_distinguishes_namespaces() {
        let first = ConfiguredProjectDiskLimaSource::new("/tmp/smolrunner-source-a").unwrap();
        let second = ConfiguredProjectDiskLimaSource::new("/tmp/smolrunner-source-b").unwrap();

        assert_ne!(first.identity(), second.identity());
        assert!(ProjectDiskLimaSourceIdentity::parse(first.identity().digest().as_str()).is_ok());
        let debug = format!("{first:?}");
        assert!(!debug.contains("/tmp/smolrunner-source-a"));
        assert!(debug.contains(first.identity().digest().as_str()));
    }

    #[test]
    fn source_identity_normalizes_equivalent_path_spellings() {
        let canonical = ConfiguredProjectDiskLimaSource::new("/tmp/smolrunner-source-a").unwrap();
        let normalized =
            ConfiguredProjectDiskLimaSource::new("/tmp//smolrunner-source-a/./").unwrap();
        assert_eq!(canonical.identity(), normalized.identity());
    }

    #[test]
    fn source_rejects_relative_parent_and_root_paths() {
        for invalid in ["relative", "/tmp/../escape", "/"] {
            assert!(ConfiguredProjectDiskLimaSource::new(invalid).is_err());
        }
    }
}
