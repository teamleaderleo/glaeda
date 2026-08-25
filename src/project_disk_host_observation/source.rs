#[cfg(target_os = "macos")]
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

use super::raw::{
    LimaStandaloneDiskName, LimaStandaloneDiskObservationRequest as RawObservationRequest,
    ProjectDiskHostObservationError,
};

const MAX_LIMA_HOME_BYTES: usize = 1_024;
const STANDALONE_DISK_COLLECTION: &str = "_disks";
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

/// Validated configured Lima namespace shared by P2 request construction and later P3 adapters.
///
/// The path stays private. On macOS the one physically established `/var -> /private/var` root
/// alias is canonicalized here after checking that exact root-owned symlink. Other path aliases are
/// left to the descriptor-bound P2 observer to reject when fresh physical evidence is acquired.
pub struct ValidatedProjectDiskLimaSource {
    canonical_lima_home: PathBuf,
    identity: ProjectDiskLimaSourceIdentity,
}

impl ValidatedProjectDiskLimaSource {
    /// Validate one configured private Lima home and derive its stable namespace identity.
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

    fn canonical_lima_home(&self) -> &Path {
        &self.canonical_lima_home
    }
}

impl fmt::Debug for ValidatedProjectDiskLimaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedProjectDiskLimaSource")
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

/// Production P2 request derived only from one validated Lima source and controller-selected disk
/// locator. Product callers cannot supply the `_disks/<locator>` directory independently.
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
        source: &ValidatedProjectDiskLimaSource,
        disk_name: LimaStandaloneDiskName,
    ) -> Result<Self, ProjectDiskHostObservationError> {
        let disk_directory = source
            .canonical_lima_home()
            .join(STANDALONE_DISK_COLLECTION)
            .join(disk_name.as_str());
        let inner = RawObservationRequest::new(
            disk_name.clone(),
            source.canonical_lima_home().to_owned(),
            disk_directory,
        )?;
        Ok(Self {
            inner,
            source_identity: source.identity().clone(),
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
        let source = ValidatedProjectDiskLimaSource::new("/tmp/smolrunner-p2-source").unwrap();
        let disk_name = LimaStandaloneDiskName::parse("srpd1-test").unwrap();
        let request =
            LimaStandaloneDiskObservationRequest::for_planned_disk(&source, disk_name).unwrap();

        assert_eq!(request.disk_name().as_str(), "srpd1-test");
        assert_eq!(request.source_identity(), source.identity());
        let debug = format!("{request:?}");
        assert!(!debug.contains("/tmp/smolrunner-p2-source"));
        assert!(debug.contains("source_identity"));
    }

    #[test]
    fn source_identity_is_path_private_and_distinguishes_namespaces() {
        let first = ValidatedProjectDiskLimaSource::new("/tmp/smolrunner-source-a").unwrap();
        let second = ValidatedProjectDiskLimaSource::new("/tmp/smolrunner-source-b").unwrap();

        assert_ne!(first.identity(), second.identity());
        assert!(ProjectDiskLimaSourceIdentity::parse(first.identity().digest().as_str()).is_ok());
        let debug = format!("{first:?}");
        assert!(!debug.contains("/tmp/smolrunner-source-a"));
        assert!(debug.contains(first.identity().digest().as_str()));
    }

    #[test]
    fn source_identity_normalizes_equivalent_path_spellings() {
        let canonical = ValidatedProjectDiskLimaSource::new("/tmp/smolrunner-source-a").unwrap();
        let normalized =
            ValidatedProjectDiskLimaSource::new("/tmp//smolrunner-source-a/./").unwrap();
        assert_eq!(canonical.identity(), normalized.identity());
    }

    #[test]
    fn source_rejects_relative_parent_and_root_paths() {
        for invalid in ["relative", "/tmp/../escape", "/"] {
            assert!(ValidatedProjectDiskLimaSource::new(invalid).is_err());
        }
    }
}
