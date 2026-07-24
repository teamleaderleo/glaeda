from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected {label} once, found {text.count(old)}")
    return text.replace(old, new, 1)


path = Path("src/runner_account_observation.rs")
text = path.read_text()
text = replace_once(
    text,
    "use std::path::{Path, PathBuf};\n",
    "use std::path::{Component, Path, PathBuf};\n",
    "path imports",
)

old_new = '''    #[must_use]
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
new_new = '''    /// Build relocated observation paths for an explicitly trusted host root.
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
text = replace_once(text, old_new, new_new, "observation paths constructor")

marker = '''impl From<RunnerAccountPlanError> for RunnerAccountObservationError {
'''
impl_block = '''impl RunnerAccountObservationError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl From<RunnerAccountPlanError> for RunnerAccountObservationError {
'''
text = replace_once(text, marker, impl_block, "observation error conversion marker")

marker = '''fn canonical_u32(value: &str) -> Option<u32> {
'''
helper = '''fn canonical_observation_path(
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
'''
text = replace_once(text, marker, helper, "canonical_u32 marker")

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
    "test observation paths helper",
)

marker = '''    #[test]
    fn exact_getent_commands_are_absolute_and_environment_free() {
'''
test = '''    #[test]
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
    fn exact_getent_commands_are_absolute_and_environment_free() {
'''
text = replace_once(text, marker, test, "getent command test marker")

path.write_text(text)
