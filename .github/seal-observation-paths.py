from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected {label} once, found {text.count(old)}")
    return text.replace(old, new, 1)


path = Path("src/runner_account_observation.rs")
text = path.read_text()

text = replace_once(
    text,
    '''use std::fmt;
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
''',
    '''use std::fmt;
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
''',
    "filesystem imports",
)

old_constructor = '''    #[must_use]
    pub fn new(
        subordinate_uids: impl Into<PathBuf>,
        subordinate_gids: impl Into<PathBuf>,
        linger_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            subordinate_uids: subordinate_uids.into(),
            subordinate_gids: subordinate_gids.into(),
            linger_directory: linger_directory.into(),
        }
    }
'''
new_constructor = '''    /// Build relocated observation paths for an explicitly trusted host root.
    ///
    /// # Errors
    ///
    /// Returns an error unless every path is a canonical absolute path without aliases.
    pub fn new(
        subordinate_uids: impl Into<PathBuf>,
        subordinate_gids: impl Into<PathBuf>,
        linger_directory: impl Into<PathBuf>,
    ) -> Result<Self, RunnerAccountObservationError> {
        Ok(Self {
            subordinate_uids: canonical_observation_path(
                "subordinate UID authority",
                subordinate_uids.into(),
            )?,
            subordinate_gids: canonical_observation_path(
                "subordinate GID authority",
                subordinate_gids.into(),
            )?,
            linger_directory: canonical_observation_path(
                "linger directory",
                linger_directory.into(),
            )?,
        })
    }
'''
text = replace_once(text, old_constructor, new_constructor, "observation paths constructor")

text = replace_once(
    text,
    '''impl From<RunnerAccountPlanError> for RunnerAccountObservationError {
''',
    '''impl RunnerAccountObservationError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl From<RunnerAccountPlanError> for RunnerAccountObservationError {
''',
    "observation error conversion",
)

text = replace_once(
    text,
    '''fn canonical_u32(value: &str) -> Option<u32> {
''',
    '''fn canonical_observation_path(
    field: &str,
    path: PathBuf,
) -> Result<PathBuf, RunnerAccountObservationError> {
    let Some(value) = path.to_str() else {
        return Err(RunnerAccountObservationError::single(format!(
            "{field} must be valid UTF-8"
        )));
    };
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(RunnerAccountObservationError::single(format!(
            "{field} must be a canonical non-root absolute path"
        )));
    }
    Ok(path)
}

fn canonical_u32(value: &str) -> Option<u32> {
''',
    "canonical path helper",
)

start = text.index("impl AccountFilesystem for LinuxAccountFilesystem {")
end = text.index("\n#[cfg(test)]", start)
filesystem_impl = '''impl AccountFilesystem for LinuxAccountFilesystem {
    fn inspect(&self, path: &Path) -> PathObservation {
        let descriptor = match open_traversed(path, OFlags::PATH) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return PathObservation::Missing,
            Err(_) => return PathObservation::Unknown,
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return PathObservation::Unknown,
        };
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => ObservedPathKind::File,
            FileType::Directory => ObservedPathKind::Directory,
            _ => ObservedPathKind::Other,
        };
        let Ok(size) = u64::try_from(stat.st_size) else {
            return PathObservation::Unknown;
        };
        PathObservation::Present(ObservedPathMetadata {
            kind,
            uid: stat.st_uid,
            gid: stat.st_gid,
            mode: stat.st_mode & 0o7777,
            size,
            nlink: stat.st_nlink,
        })
    }

    fn read_trusted(&self, path: &Path, max_bytes: usize) -> TrustedFile {
        let descriptor = match open_traversed(path, OFlags::RDONLY) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return TrustedFile::Missing,
            Err(_) => return TrustedFile::Unknown,
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return TrustedFile::Unknown,
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != 0
            || stat.st_gid != 0
            || stat.st_nlink != 1
            || stat.st_mode & 0o022 != 0
            || stat.st_size < 0
            || usize::try_from(stat.st_size).map_or(true, |size| size > max_bytes)
        {
            return TrustedFile::Unknown;
        }
        let mut bytes = Vec::new();
        if std::fs::File::from(descriptor)
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > max_bytes
        {
            return TrustedFile::Unknown;
        }
        match String::from_utf8(bytes) {
            Ok(value) if !value.contains('\\0') && (value.is_empty() || value.ends_with('\\n')) => {
                TrustedFile::Present(value)
            }
            _ => TrustedFile::Unknown,
        }
    }
}

fn open_traversed(path: &Path, final_flags: OFlags) -> Result<OwnedFd, Errno> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    let mut current = rustix_fs::open(
        "/",
        OFlags::PATH
            .union(OFlags::DIRECTORY)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )?;
    let mut remaining = components.peekable();
    while let Some(component) = remaining.next() {
        let Component::Normal(name) = component else {
            return Err(Errno::INVAL);
        };
        let flags = if remaining.peek().is_some() {
            OFlags::PATH
                .union(OFlags::DIRECTORY)
                .union(OFlags::NOFOLLOW)
                .union(OFlags::CLOEXEC)
        } else {
            final_flags.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC)
        };
        current = rustix_fs::openat(&current, name, flags, Mode::empty())?;
    }
    Ok(current)
}
'''
text = text[:start] + filesystem_impl + text[end:]

text = replace_once(
    text,
    '''    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::path::{Path, PathBuf};
''',
    '''    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::io;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
''',
    "test imports",
)

text = replace_once(
    text,
    '''        AccountFilesystem, GETENT, ObservedPathKind, ObservedPathMetadata, PathObservation,
        RunnerAccountObservationPaths, TrustedFile, getent_command, observe_with,
''',
    '''        AccountFilesystem, GETENT, LinuxAccountFilesystem, ObservedPathKind,
        ObservedPathMetadata, PathObservation, RunnerAccountObservationPaths, TrustedFile,
        getent_command, observe_with,
''',
    "test super imports",
)

text = replace_once(
    text,
    '''    fn paths() -> RunnerAccountObservationPaths {
        RunnerAccountObservationPaths::new("/test/subuid", "/test/subgid", "/test/linger")
    }
''',
    '''    fn paths() -> RunnerAccountObservationPaths {
        RunnerAccountObservationPaths::new("/test/subuid", "/test/subgid", "/test/linger")
            .expect("observation paths")
    }
''',
    "test paths helper",
)

marker = '''    #[test]
    fn exact_getent_commands_are_absolute_and_environment_free() {
'''
tests = '''    #[test]
    fn relocated_observation_paths_must_be_canonical_and_absolute() {
        RunnerAccountObservationPaths::new("relative/subuid", "/test/subgid", "/test/linger")
            .expect_err("relative authority path");
        RunnerAccountObservationPaths::new(
            "/test/subuid",
            "/test/../subgid",
            "/test/linger",
        )
        .expect_err("aliased authority path");
    }

    #[test]
    fn linux_filesystem_rejects_symlinked_parent_traversal() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "smolrunner-account-observation-{}-{suffix}",
            std::process::id()
        ));
        let real = root.join("real");
        fs::create_dir_all(&real).expect("create real directory");
        fs::write(real.join("marker"), b"").expect("create marker");
        symlink(&real, root.join("link")).expect("create parent symlink");

        let observation = LinuxAccountFilesystem.inspect(&root.join("link/marker"));
        assert_eq!(observation, PathObservation::Unknown);

        fs::remove_dir_all(&root).expect("remove test tree");
    }

    #[test]
    fn exact_getent_commands_are_absolute_and_environment_free() {
'''
text = replace_once(text, marker, tests, "final test marker")

path.write_text(text)
