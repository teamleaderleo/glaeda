use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::rootless_podman_config::{
    RootlessPodmanContainersConfig, RootlessPodmanStorageConfig,
};

pub const ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootlessPodmanConfigSourceState<T> {
    Missing,
    Present(T),
    Unknown { evidence: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootlessPodmanConfigSource<T> {
    path: PathBuf,
    state: RootlessPodmanConfigSourceState<T>,
}

impl<T> RootlessPodmanConfigSource<T> {
    /// Build one reviewed configuration source.
    ///
    /// # Errors
    ///
    /// Returns an error unless the source path is a canonical non-root absolute path.
    pub fn new(
        path: impl Into<PathBuf>,
        state: RootlessPodmanConfigSourceState<T>,
    ) -> Result<Self, String> {
        Ok(Self {
            path: canonical_absolute_path(path.into())?,
            state,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn state(&self) -> &RootlessPodmanConfigSourceState<T> {
        &self.state
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootlessPodmanConfigContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    xdg_data_home: PathBuf,
    xdg_runtime_dir: PathBuf,
}

impl RootlessPodmanConfigContext {
    /// Build the reviewed path-expansion context for one exact runner identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless every path is canonical, absolute, non-root, and the XDG config and
    /// data roots remain beneath the reviewed runner home.
    pub fn new(
        home: impl Into<PathBuf>,
        xdg_config_home: impl Into<PathBuf>,
        xdg_data_home: impl Into<PathBuf>,
        xdg_runtime_dir: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let home = canonical_absolute_path(home.into())?;
        let xdg_config_home = canonical_absolute_path(xdg_config_home.into())?;
        let xdg_data_home = canonical_absolute_path(xdg_data_home.into())?;
        let xdg_runtime_dir = canonical_absolute_path(xdg_runtime_dir.into())?;
        if !xdg_config_home.starts_with(&home) {
            return Err("XDG config home must remain beneath the reviewed runner home".to_owned());
        }
        if !xdg_data_home.starts_with(&home) {
            return Err("XDG data home must remain beneath the reviewed runner home".to_owned());
        }
        Ok(Self {
            home,
            xdg_config_home,
            xdg_data_home,
            xdg_runtime_dir,
        })
    }

    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    #[must_use]
    pub fn xdg_config_home(&self) -> &Path {
        &self.xdg_config_home
    }

    #[must_use]
    pub fn xdg_data_home(&self) -> &Path {
        &self.xdg_data_home
    }

    #[must_use]
    pub fn xdg_runtime_dir(&self) -> &Path {
        &self.xdg_runtime_dir
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanResolvedValueState {
    Known,
    Unspecified,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanResolvedValue {
    pub state: RootlessPodmanResolvedValueState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    pub evidence: Vec<String>,
}

impl RootlessPodmanResolvedValue {
    fn known(value: impl Into<String>, source: &Path, evidence: impl Into<String>) -> Self {
        Self {
            state: RootlessPodmanResolvedValueState::Known,
            value: Some(value.into()),
            source: Some(source.to_path_buf()),
            evidence: vec![evidence.into()],
        }
    }

    fn unspecified(evidence: impl Into<String>) -> Self {
        Self {
            state: RootlessPodmanResolvedValueState::Unspecified,
            value: None,
            source: None,
            evidence: vec![evidence.into()],
        }
    }

    fn unknown(source: &Path, evidence: impl Into<String>) -> Self {
        Self {
            state: RootlessPodmanResolvedValueState::Unknown,
            value: None,
            source: Some(source.to_path_buf()),
            evidence: vec![evidence.into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanResolvedContainersConfig {
    pub cgroup_manager: RootlessPodmanResolvedValue,
    pub network_backend: RootlessPodmanResolvedValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanResolvedStorageConfig {
    pub driver: RootlessPodmanResolvedValue,
    pub graph_root: RootlessPodmanResolvedValue,
    pub run_root: RootlessPodmanResolvedValue,
    pub overlay_mount_program: RootlessPodmanResolvedValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanResolvedConfig {
    pub schema_version: u8,
    pub containers: RootlessPodmanResolvedContainersConfig,
    pub storage: RootlessPodmanResolvedStorageConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigField {
    StorageDriver,
    GraphRoot,
    RunRoot,
    OverlayMountProgram,
    CgroupManager,
    NetworkBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigAssessmentState {
    Matching,
    Absent,
    Unknown,
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigFieldAssessment {
    pub field: RootlessPodmanConfigField,
    pub state: RootlessPodmanConfigAssessmentState,
    pub expected: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigAssessment {
    pub schema_version: u8,
    pub state: RootlessPodmanConfigAssessmentState,
    pub fields: Vec<RootlessPodmanConfigFieldAssessment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootlessPodmanConfigPolicy {
    storage_driver: String,
    graph_root: PathBuf,
    run_root: PathBuf,
    overlay_mount_program: PathBuf,
    cgroup_manager: String,
    network_backend: String,
}

impl RootlessPodmanConfigPolicy {
    /// Build one explicit static-preflight policy.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or unsafe identifiers or non-canonical policy paths.
    pub fn new(
        storage_driver: impl Into<String>,
        graph_root: impl Into<PathBuf>,
        run_root: impl Into<PathBuf>,
        overlay_mount_program: impl Into<PathBuf>,
        cgroup_manager: impl Into<String>,
        network_backend: impl Into<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            storage_driver: reviewed_identifier("storage driver", storage_driver.into())?,
            graph_root: canonical_absolute_path(graph_root.into())?,
            run_root: canonical_absolute_path(run_root.into())?,
            overlay_mount_program: canonical_absolute_path(overlay_mount_program.into())?,
            cgroup_manager: reviewed_identifier("cgroup manager", cgroup_manager.into())?,
            network_backend: reviewed_identifier("network backend", network_backend.into())?,
        })
    }
}

/// Resolve reviewed rootless Podman configuration sources without reading files or executing Podman.
#[must_use]
pub fn resolve_rootless_podman_config(
    context: &RootlessPodmanConfigContext,
    vendor_containers: &RootlessPodmanConfigSource<RootlessPodmanContainersConfig>,
    system_containers: &RootlessPodmanConfigSource<RootlessPodmanContainersConfig>,
    runner_containers: &RootlessPodmanConfigSource<RootlessPodmanContainersConfig>,
    system_storage: &RootlessPodmanConfigSource<RootlessPodmanStorageConfig>,
    runner_storage: &RootlessPodmanConfigSource<RootlessPodmanStorageConfig>,
) -> RootlessPodmanResolvedConfig {
    RootlessPodmanResolvedConfig {
        schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
        containers: resolve_containers([
            vendor_containers,
            system_containers,
            runner_containers,
        ]),
        storage: resolve_storage(context, system_storage, runner_storage),
    }
}

#[must_use]
pub fn assess_rootless_podman_config(
    resolved: &RootlessPodmanResolvedConfig,
    policy: &RootlessPodmanConfigPolicy,
) -> RootlessPodmanConfigAssessment {
    let fields = vec![
        assess_field(
            RootlessPodmanConfigField::StorageDriver,
            &resolved.storage.driver,
            &policy.storage_driver,
        ),
        assess_field(
            RootlessPodmanConfigField::GraphRoot,
            &resolved.storage.graph_root,
            &policy.graph_root.display().to_string(),
        ),
        assess_field(
            RootlessPodmanConfigField::RunRoot,
            &resolved.storage.run_root,
            &policy.run_root.display().to_string(),
        ),
        assess_field(
            RootlessPodmanConfigField::OverlayMountProgram,
            &resolved.storage.overlay_mount_program,
            &policy.overlay_mount_program.display().to_string(),
        ),
        assess_field(
            RootlessPodmanConfigField::CgroupManager,
            &resolved.containers.cgroup_manager,
            &policy.cgroup_manager,
        ),
        assess_field(
            RootlessPodmanConfigField::NetworkBackend,
            &resolved.containers.network_backend,
            &policy.network_backend,
        ),
    ];
    let state = fields
        .iter()
        .map(|field| field.state)
        .max()
        .unwrap_or(RootlessPodmanConfigAssessmentState::Matching);
    RootlessPodmanConfigAssessment {
        schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
        state,
        fields,
    }
}

fn resolve_containers(
    sources: [&RootlessPodmanConfigSource<RootlessPodmanContainersConfig>; 3],
) -> RootlessPodmanResolvedContainersConfig {
    let mut resolved = RootlessPodmanResolvedContainersConfig {
        cgroup_manager: RootlessPodmanResolvedValue::unspecified(
            "no reviewed containers.conf source defines cgroup_manager",
        ),
        network_backend: RootlessPodmanResolvedValue::unspecified(
            "no reviewed containers.conf source defines network_backend",
        ),
    };
    for source in sources {
        match source.state() {
            RootlessPodmanConfigSourceState::Missing => {}
            RootlessPodmanConfigSourceState::Unknown { evidence } => {
                resolved.cgroup_manager = RootlessPodmanResolvedValue::unknown(
                    source.path(),
                    format!("source may override cgroup_manager but is unknown: {evidence}"),
                );
                resolved.network_backend = RootlessPodmanResolvedValue::unknown(
                    source.path(),
                    format!("source may override network_backend but is unknown: {evidence}"),
                );
            }
            RootlessPodmanConfigSourceState::Present(config) => {
                if let Some(value) = &config.cgroup_manager {
                    resolved.cgroup_manager = RootlessPodmanResolvedValue::known(
                        value,
                        source.path(),
                        "later containers.conf source defines cgroup_manager",
                    );
                }
                if let Some(value) = &config.network_backend {
                    resolved.network_backend = RootlessPodmanResolvedValue::known(
                        value,
                        source.path(),
                        "later containers.conf source defines network_backend",
                    );
                }
            }
        }
    }
    resolved
}

fn resolve_storage(
    context: &RootlessPodmanConfigContext,
    system: &RootlessPodmanConfigSource<RootlessPodmanStorageConfig>,
    runner: &RootlessPodmanConfigSource<RootlessPodmanStorageConfig>,
) -> RootlessPodmanResolvedStorageConfig {
    match runner.state() {
        RootlessPodmanConfigSourceState::Present(config) => {
            resolve_selected_storage(context, runner.path(), config, true)
        }
        RootlessPodmanConfigSourceState::Unknown { evidence } => unknown_storage(
            runner.path(),
            format!("higher-precedence runner storage.conf is unknown: {evidence}"),
        ),
        RootlessPodmanConfigSourceState::Missing => match system.state() {
            RootlessPodmanConfigSourceState::Present(config) => {
                resolve_selected_storage(context, system.path(), config, false)
            }
            RootlessPodmanConfigSourceState::Unknown { evidence } => unknown_storage(
                system.path(),
                format!("selected system storage.conf is unknown: {evidence}"),
            ),
            RootlessPodmanConfigSourceState::Missing => RootlessPodmanResolvedStorageConfig {
                driver: RootlessPodmanResolvedValue::unspecified(
                    "no reviewed storage.conf source defines a driver",
                ),
                graph_root: RootlessPodmanResolvedValue::unspecified(
                    "no reviewed storage.conf source defines an explicit rootless graph root",
                ),
                run_root: RootlessPodmanResolvedValue::unspecified(
                    "no reviewed storage.conf source defines an explicit rootless run root",
                ),
                overlay_mount_program: RootlessPodmanResolvedValue::unspecified(
                    "no reviewed storage.conf source defines an overlay mount program",
                ),
            },
        },
    }
}

fn resolve_selected_storage(
    context: &RootlessPodmanConfigContext,
    source: &Path,
    config: &RootlessPodmanStorageConfig,
    runner_specific: bool,
) -> RootlessPodmanResolvedStorageConfig {
    let driver = config.driver.as_ref().map_or_else(
        || RootlessPodmanResolvedValue::unspecified("selected storage.conf does not define driver"),
        |value| {
            RootlessPodmanResolvedValue::known(
                value,
                source,
                "selected storage.conf defines driver",
            )
        },
    );
    let overlay_mount_program = config.overlay_mount_program.as_ref().map_or_else(
        || {
            RootlessPodmanResolvedValue::unspecified(
                "selected storage.conf does not define overlay mount_program",
            )
        },
        |value| resolve_path_value(context, source, value, "overlay mount_program"),
    );

    let (graph_root, run_root) = if runner_specific {
        (
            select_runner_graph_root(context, source, config),
            config.runroot.as_ref().map_or_else(
                || {
                    RootlessPodmanResolvedValue::unspecified(
                        "runner storage.conf does not define runroot",
                    )
                },
                |value| resolve_path_value(context, source, value, "runner runroot"),
            ),
        )
    } else {
        (
            config.rootless_storage_path.as_ref().map_or_else(
                || {
                    RootlessPodmanResolvedValue::unspecified(
                        "system storage.conf does not define rootless_storage_path; system graphroot is ignored for rootless Podman",
                    )
                },
                |value| resolve_path_value(context, source, value, "system rootless_storage_path"),
            ),
            RootlessPodmanResolvedValue::unspecified(
                "system storage.conf runroot is ignored for rootless Podman",
            ),
        )
    };

    RootlessPodmanResolvedStorageConfig {
        driver,
        graph_root,
        run_root,
        overlay_mount_program,
    }
}

fn select_runner_graph_root(
    context: &RootlessPodmanConfigContext,
    source: &Path,
    config: &RootlessPodmanStorageConfig,
) -> RootlessPodmanResolvedValue {
    match (&config.graphroot, &config.rootless_storage_path) {
        (Some(graphroot), None) => {
            resolve_path_value(context, source, graphroot, "runner graphroot")
        }
        (None, Some(rootless_storage_path)) => resolve_path_value(
            context,
            source,
            rootless_storage_path,
            "runner rootless_storage_path",
        ),
        (None, None) => RootlessPodmanResolvedValue::unspecified(
            "runner storage.conf does not define graphroot or rootless_storage_path",
        ),
        (Some(_), Some(_)) => RootlessPodmanResolvedValue::unknown(
            source,
            "runner storage.conf defines both graphroot and rootless_storage_path; reviewed precedence is ambiguous",
        ),
    }
}

fn resolve_path_value(
    context: &RootlessPodmanConfigContext,
    source: &Path,
    raw: &str,
    field: &str,
) -> RootlessPodmanResolvedValue {
    match expand_reviewed_path(context, raw) {
        Ok(path) => RootlessPodmanResolvedValue::known(
            path.display().to_string(),
            source,
            format!("{field} expands to a canonical reviewed path"),
        ),
        Err(problem) => RootlessPodmanResolvedValue::unknown(
            source,
            format!("{field} could not be expanded safely: {problem}"),
        ),
    }
}

fn unknown_storage(
    source: &Path,
    evidence: impl Into<String>,
) -> RootlessPodmanResolvedStorageConfig {
    let evidence = evidence.into();
    RootlessPodmanResolvedStorageConfig {
        driver: RootlessPodmanResolvedValue::unknown(source, evidence.clone()),
        graph_root: RootlessPodmanResolvedValue::unknown(source, evidence.clone()),
        run_root: RootlessPodmanResolvedValue::unknown(source, evidence.clone()),
        overlay_mount_program: RootlessPodmanResolvedValue::unknown(source, evidence),
    }
}

fn assess_field(
    field: RootlessPodmanConfigField,
    resolved: &RootlessPodmanResolvedValue,
    expected: &str,
) -> RootlessPodmanConfigFieldAssessment {
    let (state, observed, mut evidence) = match resolved.state {
        RootlessPodmanResolvedValueState::Known => {
            let observed = resolved.value.clone();
            let state = if observed.as_deref() == Some(expected) {
                RootlessPodmanConfigAssessmentState::Matching
            } else {
                RootlessPodmanConfigAssessmentState::Conflicting
            };
            (state, observed, resolved.evidence.clone())
        }
        RootlessPodmanResolvedValueState::Unspecified => (
            RootlessPodmanConfigAssessmentState::Absent,
            None,
            resolved.evidence.clone(),
        ),
        RootlessPodmanResolvedValueState::Unknown => (
            RootlessPodmanConfigAssessmentState::Unknown,
            None,
            resolved.evidence.clone(),
        ),
    };
    evidence.push(match state {
        RootlessPodmanConfigAssessmentState::Matching => {
            format!("resolved value matches expected policy value {expected:?}")
        }
        RootlessPodmanConfigAssessmentState::Absent => {
            format!("required policy value {expected:?} is not explicitly resolved")
        }
        RootlessPodmanConfigAssessmentState::Unknown => {
            format!("required policy value {expected:?} cannot be compared safely")
        }
        RootlessPodmanConfigAssessmentState::Conflicting => {
            format!("resolved value does not match expected policy value {expected:?}")
        }
    });
    RootlessPodmanConfigFieldAssessment {
        field,
        state,
        expected: expected.to_owned(),
        observed,
        evidence,
    }
}

fn expand_reviewed_path(
    context: &RootlessPodmanConfigContext,
    raw: &str,
) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.len() > 4_096 || raw.chars().any(char::is_control) {
        return Err(
            "configuration path is empty, oversized, or contains a control character".to_owned(),
        );
    }
    if raw.starts_with('~') {
        return Err("tilde expansion is not reviewed".to_owned());
    }
    let replacements = [
        ("${XDG_RUNTIME_DIR}", context.xdg_runtime_dir()),
        ("$XDG_RUNTIME_DIR", context.xdg_runtime_dir()),
        ("${XDG_CONFIG_HOME}", context.xdg_config_home()),
        ("$XDG_CONFIG_HOME", context.xdg_config_home()),
        ("${XDG_DATA_HOME}", context.xdg_data_home()),
        ("$XDG_DATA_HOME", context.xdg_data_home()),
        ("${HOME}", context.home()),
        ("$HOME", context.home()),
    ];
    let mut expanded = None;
    for (prefix, replacement) in replacements {
        if raw == prefix {
            expanded = Some(replacement.to_path_buf());
            break;
        }
        if let Some(suffix) = raw
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.strip_prefix('/'))
        {
            expanded = Some(replacement.join(suffix));
            break;
        }
    }
    let path = match expanded {
        Some(path) => path,
        None if raw.contains('$') => {
            return Err("configuration path uses an unreviewed variable".to_owned());
        }
        None => PathBuf::from(raw),
    };
    canonical_absolute_path(path)
}

fn canonical_absolute_path(path: PathBuf) -> Result<PathBuf, String> {
    let Some(value) = path.to_str() else {
        return Err("path must be valid UTF-8".to_owned());
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
        return Err("path must be a canonical non-root absolute path".to_owned());
    }
    Ok(path)
}

fn reviewed_identifier(label: &str, value: String) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!(
            "{label} must be a bounded ASCII identifier using letters, digits, dot, underscore, or dash"
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source<T>(
        path: &str,
        state: RootlessPodmanConfigSourceState<T>,
    ) -> RootlessPodmanConfigSource<T> {
        RootlessPodmanConfigSource::new(path, state).expect("source")
    }

    fn context() -> RootlessPodmanConfigContext {
        RootlessPodmanConfigContext::new(
            "/var/lib/project-runner",
            "/var/lib/project-runner/.config",
            "/var/lib/project-runner/.local/share",
            "/run/user/1001",
        )
        .expect("context")
    }

    fn policy() -> RootlessPodmanConfigPolicy {
        RootlessPodmanConfigPolicy::new(
            "overlay",
            "/var/lib/project-runner/.local/share/containers/storage",
            "/run/user/1001/containers",
            "/usr/bin/fuse-overlayfs",
            "systemd",
            "netavark",
        )
        .expect("policy")
    }

    fn containers(
        cgroup_manager: Option<&str>,
        network_backend: Option<&str>,
    ) -> RootlessPodmanContainersConfig {
        RootlessPodmanContainersConfig {
            cgroup_manager: cgroup_manager.map(str::to_owned),
            network_backend: network_backend.map(str::to_owned),
        }
    }

    fn storage(
        driver: Option<&str>,
        runroot: Option<&str>,
        graphroot: Option<&str>,
        rootless_storage_path: Option<&str>,
        overlay_mount_program: Option<&str>,
    ) -> RootlessPodmanStorageConfig {
        RootlessPodmanStorageConfig {
            driver: driver.map(str::to_owned),
            runroot: runroot.map(str::to_owned),
            graphroot: graphroot.map(str::to_owned),
            rootless_storage_path: rootless_storage_path.map(str::to_owned),
            overlay_mount_program: overlay_mount_program.map(str::to_owned),
        }
    }

    #[test]
    fn containers_sources_override_fields_in_reviewed_order() {
        let resolved = resolve_rootless_podman_config(
            &context(),
            &source(
                "/usr/share/containers/containers.conf",
                RootlessPodmanConfigSourceState::Present(containers(
                    Some("cgroupfs"),
                    Some("cni"),
                )),
            ),
            &source(
                "/etc/containers/containers.conf",
                RootlessPodmanConfigSourceState::Present(containers(Some("systemd"), None)),
            ),
            &source(
                "/var/lib/project-runner/.config/containers/containers.conf",
                RootlessPodmanConfigSourceState::Present(containers(None, Some("netavark"))),
            ),
            &source(
                "/etc/containers/storage.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/var/lib/project-runner/.config/containers/storage.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
        );

        assert_eq!(
            resolved.containers.cgroup_manager.value.as_deref(),
            Some("systemd")
        );
        assert_eq!(
            resolved.containers.network_backend.value.as_deref(),
            Some("netavark")
        );
    }

    #[test]
    fn later_explicit_field_can_clear_earlier_unknown_for_that_field_only() {
        let resolved = resolve_rootless_podman_config(
            &context(),
            &source(
                "/usr/share/containers/containers.conf",
                RootlessPodmanConfigSourceState::Present(containers(
                    Some("cgroupfs"),
                    Some("cni"),
                )),
            ),
            &source(
                "/etc/containers/containers.conf",
                RootlessPodmanConfigSourceState::Unknown {
                    evidence: "permission denied".to_owned(),
                },
            ),
            &source(
                "/var/lib/project-runner/.config/containers/containers.conf",
                RootlessPodmanConfigSourceState::Present(containers(Some("systemd"), None)),
            ),
            &source(
                "/etc/containers/storage.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/var/lib/project-runner/.config/containers/storage.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
        );

        assert_eq!(
            resolved.containers.cgroup_manager.state,
            RootlessPodmanResolvedValueState::Known
        );
        assert_eq!(
            resolved.containers.network_backend.state,
            RootlessPodmanResolvedValueState::Unknown
        );
    }

    #[test]
    fn runner_storage_replaces_system_storage_instead_of_inheriting_fields() {
        let resolved = resolve_rootless_podman_config(
            &context(),
            &source(
                "/usr/share/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/etc/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/var/lib/project-runner/.config/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/etc/containers/storage.conf",
                RootlessPodmanConfigSourceState::Present(storage(
                    Some("overlay"),
                    Some("/run/containers/storage"),
                    Some("/var/lib/containers/storage"),
                    None,
                    Some("/usr/bin/fuse-overlayfs"),
                )),
            ),
            &source(
                "/var/lib/project-runner/.config/containers/storage.conf",
                RootlessPodmanConfigSourceState::Present(storage(
                    Some("vfs"),
                    Some("$XDG_RUNTIME_DIR/containers"),
                    Some("$XDG_DATA_HOME/containers/storage"),
                    None,
                    None,
                )),
            ),
        );

        assert_eq!(resolved.storage.driver.value.as_deref(), Some("vfs"));
        assert_eq!(
            resolved.storage.overlay_mount_program.state,
            RootlessPodmanResolvedValueState::Unspecified
        );
    }

    #[test]
    fn unknown_runner_storage_hides_known_system_storage() {
        let resolved = resolve_rootless_podman_config(
            &context(),
            &source(
                "/usr/share/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/etc/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/var/lib/project-runner/.config/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/etc/containers/storage.conf",
                RootlessPodmanConfigSourceState::Present(storage(
                    Some("overlay"),
                    None,
                    None,
                    Some("$HOME/.local/share/containers/storage"),
                    Some("/usr/bin/fuse-overlayfs"),
                )),
            ),
            &source(
                "/var/lib/project-runner/.config/containers/storage.conf",
                RootlessPodmanConfigSourceState::Unknown {
                    evidence: "unsafe metadata".to_owned(),
                },
            ),
        );

        assert_eq!(
            resolved.storage.driver.state,
            RootlessPodmanResolvedValueState::Unknown
        );
        assert_eq!(
            resolved.storage.graph_root.state,
            RootlessPodmanResolvedValueState::Unknown
        );
    }

    #[test]
    fn system_graphroot_and_runroot_do_not_authorize_rootless_paths() {
        let resolved = resolve_rootless_podman_config(
            &context(),
            &source(
                "/usr/share/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/etc/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/var/lib/project-runner/.config/containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
            &source(
                "/etc/containers/storage.conf",
                RootlessPodmanConfigSourceState::Present(storage(
                    Some("overlay"),
                    Some("/run/containers/storage"),
                    Some("/var/lib/containers/storage"),
                    Some("$XDG_DATA_HOME/containers/storage"),
                    Some("/usr/bin/fuse-overlayfs"),
                )),
            ),
            &source(
                "/var/lib/project-runner/.config/containers/storage.conf",
                RootlessPodmanConfigSourceState::Missing,
            ),
        );

        assert_eq!(
            resolved.storage.graph_root.value.as_deref(),
            Some("/var/lib/project-runner/.local/share/containers/storage")
        );
        assert_eq!(
            resolved.storage.run_root.state,
            RootlessPodmanResolvedValueState::Unspecified
        );
    }

    #[test]
    fn reviewed_variables_expand_and_unreviewed_variables_fail_closed() {
        let good = expand_reviewed_path(&context(), "$XDG_RUNTIME_DIR/containers")
            .expect("reviewed expansion");
        assert_eq!(good, Path::new("/run/user/1001/containers"));

        for value in [
            "$UID/containers",
            "${USER}/containers",
            "~/containers",
            "$HOME/../escape",
            "relative/path",
        ] {
            assert!(expand_reviewed_path(&context(), value).is_err(), "{value}");
        }
    }

    #[test]
    fn matching_policy_is_ready_and_mismatch_is_conflicting() {
        let matching = RootlessPodmanResolvedConfig {
            schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
            containers: RootlessPodmanResolvedContainersConfig {
                cgroup_manager: RootlessPodmanResolvedValue::known(
                    "systemd",
                    Path::new("/etc/containers/containers.conf"),
                    "test",
                ),
                network_backend: RootlessPodmanResolvedValue::known(
                    "netavark",
                    Path::new("/etc/containers/containers.conf"),
                    "test",
                ),
            },
            storage: RootlessPodmanResolvedStorageConfig {
                driver: RootlessPodmanResolvedValue::known(
                    "overlay",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
                graph_root: RootlessPodmanResolvedValue::known(
                    "/var/lib/project-runner/.local/share/containers/storage",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
                run_root: RootlessPodmanResolvedValue::known(
                    "/run/user/1001/containers",
                    Path::new("/var/lib/project-runner/.config/containers/storage.conf"),
                    "test",
                ),
                overlay_mount_program: RootlessPodmanResolvedValue::known(
                    "/usr/bin/fuse-overlayfs",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
            },
        };
        let assessment = assess_rootless_podman_config(&matching, &policy());
        assert_eq!(
            assessment.state,
            RootlessPodmanConfigAssessmentState::Matching
        );

        let mut conflicting = matching;
        conflicting.containers.network_backend.value = Some("cni".to_owned());
        let assessment = assess_rootless_podman_config(&conflicting, &policy());
        assert_eq!(
            assessment.state,
            RootlessPodmanConfigAssessmentState::Conflicting
        );
    }

    #[test]
    fn unspecified_and_unknown_values_remain_distinct() {
        let resolved = RootlessPodmanResolvedConfig {
            schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
            containers: RootlessPodmanResolvedContainersConfig {
                cgroup_manager: RootlessPodmanResolvedValue::unspecified("missing"),
                network_backend: RootlessPodmanResolvedValue::unknown(
                    Path::new("/etc/containers/containers.conf"),
                    "unreadable",
                ),
            },
            storage: RootlessPodmanResolvedStorageConfig {
                driver: RootlessPodmanResolvedValue::known(
                    "overlay",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
                graph_root: RootlessPodmanResolvedValue::known(
                    "/var/lib/project-runner/.local/share/containers/storage",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
                run_root: RootlessPodmanResolvedValue::known(
                    "/run/user/1001/containers",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
                overlay_mount_program: RootlessPodmanResolvedValue::known(
                    "/usr/bin/fuse-overlayfs",
                    Path::new("/etc/containers/storage.conf"),
                    "test",
                ),
            },
        };
        let assessment = assess_rootless_podman_config(&resolved, &policy());
        let cgroup = assessment
            .fields
            .iter()
            .find(|field| field.field == RootlessPodmanConfigField::CgroupManager)
            .expect("cgroup field");
        let network = assessment
            .fields
            .iter()
            .find(|field| field.field == RootlessPodmanConfigField::NetworkBackend)
            .expect("network field");
        assert_eq!(cgroup.state, RootlessPodmanConfigAssessmentState::Absent);
        assert_eq!(network.state, RootlessPodmanConfigAssessmentState::Unknown);
        assert_eq!(
            assessment.state,
            RootlessPodmanConfigAssessmentState::Unknown
        );
    }

    #[test]
    fn context_rejects_xdg_roots_outside_runner_home() {
        assert!(
            RootlessPodmanConfigContext::new(
                "/var/lib/project-runner",
                "/etc/project-runner",
                "/var/lib/project-runner/.local/share",
                "/run/user/1001",
            )
            .is_err()
        );
    }

    #[test]
    fn source_and_policy_paths_must_be_canonical() {
        assert!(
            RootlessPodmanConfigSource::<RootlessPodmanContainersConfig>::new(
                "/etc/containers/../containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            )
            .is_err()
        );
        assert!(
            RootlessPodmanConfigPolicy::new(
                "overlay",
                "/var/lib/project-runner/../escape",
                "/run/user/1001/containers",
                "/usr/bin/fuse-overlayfs",
                "systemd",
                "netavark",
            )
            .is_err()
        );
    }
}
