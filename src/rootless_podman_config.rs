use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

mod parser;
#[cfg(test)]
mod tests;

use parser::{ConfigField, parse_relevant_fields};

pub const MAX_ROOTLESS_PODMAN_CONFIG_BYTES: usize = 65_536;
pub const MAX_ROOTLESS_PODMAN_CONFIG_LINES: usize = 2_048;
pub const MAX_ROOTLESS_PODMAN_CONFIG_LINE_BYTES: usize = 4_096;
pub const MAX_ROOTLESS_PODMAN_CONFIG_VALUE_BYTES: usize = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigKind {
    Storage,
    Containers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigErrorKind {
    Oversized,
    TooManyLines,
    LineTooLong,
    InvalidControlCharacter,
    MalformedTable,
    MalformedRelevantAssignment,
    DuplicateRelevantKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigError {
    kind: RootlessPodmanConfigErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    message: String,
}

impl RootlessPodmanConfigError {
    pub(crate) fn new(
        kind: RootlessPodmanConfigErrorKind,
        line: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            line,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RootlessPodmanConfigErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        self.line
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RootlessPodmanConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(formatter, "line {line}: {}", self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for RootlessPodmanConfigError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanStorageConfig {
    pub driver: Option<String>,
    pub runroot: Option<String>,
    pub graphroot: Option<String>,
    pub rootless_storage_path: Option<String>,
    pub overlay_mount_program: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanContainersConfig {
    pub cgroup_manager: Option<String>,
    pub network_backend: Option<String>,
}

/// Parse the bounded subset of `storage.conf` needed by static rootless Podman preflight.
///
/// Unknown tables and keys are ignored. Relevant keys must be unique and use one-line quoted TOML
/// strings. Multiline strings, computed interpolation, and unbounded values are deliberately not
/// accepted. The parser does not execute Podman or attempt to reproduce its complete configuration
/// engine.
///
/// # Errors
///
/// Returns an error for oversized input, unsafe control characters, malformed tables, malformed
/// relevant assignments, or duplicate relevant keys.
pub fn parse_rootless_podman_storage_config(
    input: &str,
) -> Result<RootlessPodmanStorageConfig, RootlessPodmanConfigError> {
    let fields = parse_relevant_fields(input, RootlessPodmanConfigKind::Storage)?;
    Ok(RootlessPodmanStorageConfig {
        driver: take(&fields, ConfigField::StorageDriver),
        runroot: take(&fields, ConfigField::StorageRunroot),
        graphroot: take(&fields, ConfigField::StorageGraphroot),
        rootless_storage_path: take(&fields, ConfigField::RootlessStoragePath),
        overlay_mount_program: take(&fields, ConfigField::OverlayMountProgram),
    })
}

/// Parse the bounded subset of `containers.conf` needed by static rootless Podman preflight.
///
/// Unknown tables and keys are ignored. Relevant keys must be unique and use one-line quoted TOML
/// strings. The absence of a key is preserved as `None`; this parser does not guess a distribution
/// or build-specific default.
///
/// # Errors
///
/// Returns an error for oversized input, unsafe control characters, malformed tables, malformed
/// relevant assignments, or duplicate relevant keys.
pub fn parse_rootless_podman_containers_config(
    input: &str,
) -> Result<RootlessPodmanContainersConfig, RootlessPodmanConfigError> {
    let fields = parse_relevant_fields(input, RootlessPodmanConfigKind::Containers)?;
    Ok(RootlessPodmanContainersConfig {
        cgroup_manager: take(&fields, ConfigField::CgroupManager),
        network_backend: take(&fields, ConfigField::NetworkBackend),
    })
}

fn take(fields: &BTreeMap<ConfigField, String>, field: ConfigField) -> Option<String> {
    fields.get(&field).cloned()
}
