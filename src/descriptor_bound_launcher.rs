use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::process::MAX_CAPTURED_STREAM_BYTES;

pub const DESCRIPTOR_BOUND_LAUNCH_SCHEMA_VERSION: u8 = 1;

pub(super) const REDACTED: &str = "[REDACTED]";
pub(super) const CAPTURE_BUFFER_BYTES: usize = 8_192;
const MAX_COMMAND_ID_BYTES: usize = 128;
const MAX_ARGUMENTS: usize = 64;
const MAX_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_VALUE_BYTES: usize = 8_192;
const MAX_TOTAL_VALUE_BYTES: usize = 65_536;
const MAX_PATH_BYTES: usize = 4_096;

#[derive(Clone, PartialEq, Eq)]
pub struct ReviewedFilesystemIdentity {
    device: u64,
    inode: u64,
    owner_uid: u32,
    owner_gid: u32,
    mode: u32,
}

impl ReviewedFilesystemIdentity {
    /// Build one exact reviewed filesystem-object identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the inode is zero or the mode contains object-type bits.
    pub fn new(
        device: u64,
        inode: u64,
        owner_uid: u32,
        owner_gid: u32,
        mode: u32,
    ) -> Result<Self, DescriptorBoundLaunchError> {
        if inode == 0 {
            return Err(DescriptorBoundLaunchError::plan(
                "filesystem_identity",
                "reviewed filesystem identity requires a nonzero inode",
            ));
        }
        if mode > 0o7777 {
            return Err(DescriptorBoundLaunchError::plan(
                "filesystem_identity",
                "reviewed filesystem mode must contain permission and special bits only",
            ));
        }
        Ok(Self {
            device,
            inode,
            owner_uid,
            owner_gid,
            mode,
        })
    }

    fn exact_match(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.owner_uid == other.owner_uid
            && self.owner_gid == other.owner_gid
            && self.mode == other.mode
    }
}

impl fmt::Debug for ReviewedFilesystemIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private reviewed filesystem identity>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ReviewedLaunchCredentials {
    Inherit {
        uid: u32,
        gid: u32,
    },
    DropPrivileges {
        launcher_uid: u32,
        launcher_gid: u32,
        target_uid: u32,
        target_gid: u32,
    },
}

impl ReviewedLaunchCredentials {
    fn validate(self) -> Result<(), DescriptorBoundLaunchError> {
        match self {
            Self::Inherit { .. } => Ok(()),
            Self::DropPrivileges { launcher_uid, .. } if launcher_uid != 0 => {
                Err(DescriptorBoundLaunchError::plan(
                    "credentials",
                    "privilege drop requires a reviewed root launcher identity",
                ))
            }
            Self::DropPrivileges {
                target_uid,
                target_gid,
                ..
            } if target_uid == 0 || target_gid == 0 => Err(DescriptorBoundLaunchError::plan(
                "credentials",
                "privilege drop requires a non-root target user and group",
            )),
            Self::DropPrivileges { .. } => Ok(()),
        }
    }

    const fn launcher_identity(self) -> (u32, u32) {
        match self {
            Self::Inherit { uid, gid } => (uid, gid),
            Self::DropPrivileges {
                launcher_uid,
                launcher_gid,
                ..
            } => (launcher_uid, launcher_gid),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ReviewedLaunchValue {
    Plain(String),
    Secret(String),
}

impl ReviewedLaunchValue {
    #[must_use]
    pub fn plain(value: impl Into<String>) -> Self {
        Self::Plain(value.into())
    }

    #[must_use]
    pub fn secret(value: impl Into<String>) -> Self {
        Self::Secret(value.into())
    }

    fn exposed(&self) -> &str {
        match self {
            Self::Plain(value) | Self::Secret(value) => value,
        }
    }

    fn secret_value(&self) -> Option<&str> {
        match self {
            Self::Plain(_) => None,
            Self::Secret(value) if value.is_empty() => None,
            Self::Secret(value) => Some(value),
        }
    }
}

impl fmt::Debug for ReviewedLaunchValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain(_) => formatter.write_str("<private reviewed value>"),
            Self::Secret(_) => formatter.write_str(REDACTED),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ReviewedLaunchObject {
    logical_path: PathBuf,
    identity: ReviewedFilesystemIdentity,
}

impl fmt::Debug for ReviewedLaunchObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private reviewed launch object>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ReviewedLinuxLaunchPlan {
    schema_version: u8,
    command_id: String,
    executable: ReviewedLaunchObject,
    working_directory: ReviewedLaunchObject,
    arguments: Vec<ReviewedLaunchValue>,
    environment: BTreeMap<String, ReviewedLaunchValue>,
    credentials: ReviewedLaunchCredentials,
}

impl ReviewedLinuxLaunchPlan {
    /// Bind one reviewed direct executable, cwd, argv, environment, and credential transition.
    ///
    /// This constructor performs no filesystem or process operation. The executor later opens the
    /// two reviewed paths with no-follow descriptor traversal and requires the exact supplied object
    /// identities before it can spawn.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid command ID, path, argument/environment bound, environment
    /// name, embedded NUL, or unsupported credential transition.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        command_id: impl Into<String>,
        executable_path: impl Into<PathBuf>,
        executable_identity: ReviewedFilesystemIdentity,
        working_directory: impl Into<PathBuf>,
        working_directory_identity: ReviewedFilesystemIdentity,
        arguments: Vec<ReviewedLaunchValue>,
        environment: BTreeMap<String, ReviewedLaunchValue>,
        credentials: ReviewedLaunchCredentials,
    ) -> Result<Self, DescriptorBoundLaunchError> {
        let command_id = command_id.into();
        validate_command_id(&command_id)?;
        let executable_path = validate_absolute_path("executable", executable_path.into())?;
        let working_directory =
            validate_absolute_path("working_directory", working_directory.into())?;
        validate_values("arguments", &arguments, MAX_ARGUMENTS)?;
        validate_environment(&environment)?;
        credentials.validate()?;

        Ok(Self {
            schema_version: DESCRIPTOR_BOUND_LAUNCH_SCHEMA_VERSION,
            command_id,
            executable: ReviewedLaunchObject {
                logical_path: executable_path,
                identity: executable_identity,
            },
            working_directory: ReviewedLaunchObject {
                logical_path: working_directory,
                identity: working_directory_identity,
            },
            arguments,
            environment,
            credentials,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    #[must_use]
    pub fn environment_keys(&self) -> Vec<String> {
        self.environment.keys().cloned().collect()
    }

    #[must_use]
    pub const fn credentials(&self) -> ReviewedLaunchCredentials {
        self.credentials
    }
}

impl fmt::Debug for ReviewedLinuxLaunchPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewedLinuxLaunchPlan")
            .field("schema_version", &self.schema_version)
            .field("command_id", &self.command_id)
            .field("executable", &"<private descriptor-bound executable>")
            .field("working_directory", &"<private descriptor-bound cwd>")
            .field("argument_count", &self.arguments.len())
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .field("credentials", &self.credentials)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DescriptorBoundTermination {
    Exited { code: u8 },
    Signaled { signal: u8 },
}

#[derive(Clone, PartialEq, Eq)]
struct DescriptorBoundPrivateDiagnostics {
    stdout: String,
    stderr: String,
}

impl DescriptorBoundPrivateDiagnostics {
    fn has_output(&self) -> bool {
        !self.stdout.is_empty() || !self.stderr.is_empty()
    }
}

impl fmt::Debug for DescriptorBoundPrivateDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DescriptorBoundPrivateDiagnostics")
            .field("stdout", &"<private diagnostic>")
            .field("stderr", &"<private diagnostic>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct DescriptorBoundPrivateEvidence {
    executable: ReviewedFilesystemIdentity,
    working_directory: ReviewedFilesystemIdentity,
}

impl fmt::Debug for DescriptorBoundPrivateEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bound_objects = usize::from(self.executable.inode != 0)
            + usize::from(self.working_directory.inode != 0);
        formatter
            .debug_struct("DescriptorBoundPrivateEvidence")
            .field("bound_objects", &bound_objects)
            .field("identities", &"<private>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DescriptorBoundLaunchReceipt {
    schema_version: u8,
    command_id: String,
    argument_count: usize,
    environment_keys: Vec<String>,
    credentials: ReviewedLaunchCredentials,
    termination: DescriptorBoundTermination,
    success: bool,
    #[serde(skip)]
    plan: ReviewedLinuxLaunchPlan,
    #[serde(skip)]
    diagnostics: DescriptorBoundPrivateDiagnostics,
    #[serde(skip)]
    evidence: DescriptorBoundPrivateEvidence,
}

impl DescriptorBoundLaunchReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    #[must_use]
    pub const fn argument_count(&self) -> usize {
        self.argument_count
    }

    #[must_use]
    pub fn environment_keys(&self) -> &[String] {
        &self.environment_keys
    }

    #[must_use]
    pub const fn credentials(&self) -> ReviewedLaunchCredentials {
        self.credentials
    }

    #[must_use]
    pub const fn termination(&self) -> DescriptorBoundTermination {
        self.termination
    }

    #[must_use]
    pub const fn success(&self) -> bool {
        self.success
    }

    #[must_use]
    pub const fn plan(&self) -> &ReviewedLinuxLaunchPlan {
        &self.plan
    }

    #[must_use]
    pub fn has_private_diagnostics(&self) -> bool {
        self.diagnostics.has_output()
    }
}

impl fmt::Debug for DescriptorBoundLaunchReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DescriptorBoundLaunchReceipt")
            .field("schema_version", &self.schema_version)
            .field("command_id", &self.command_id)
            .field("argument_count", &self.argument_count)
            .field("environment_keys", &self.environment_keys)
            .field("credentials", &self.credentials)
            .field("termination", &self.termination)
            .field("success", &self.success)
            .field("plan", &"<retained exact private launch plan>")
            .field("diagnostics", &self.diagnostics)
            .field("evidence", &self.evidence)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorBoundLaunchErrorKind {
    Plan,
    FilesystemIdentity,
    UnsupportedExecutable,
    Credentials,
    DescriptorAlias,
    Spawn,
    OutputCapture,
    OutputLimit,
    Status,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DescriptorBoundLaunchError {
    kind: DescriptorBoundLaunchErrorKind,
    stage: &'static str,
    public_message: String,
}

impl DescriptorBoundLaunchError {
    #[must_use]
    pub const fn kind(&self) -> DescriptorBoundLaunchErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(
        kind: DescriptorBoundLaunchErrorKind,
        stage: &'static str,
        public_message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            stage,
            public_message: public_message.into(),
        }
    }

    fn plan(stage: &'static str, message: impl Into<String>) -> Self {
        Self::new(DescriptorBoundLaunchErrorKind::Plan, stage, message)
    }

    fn identity(stage: &'static str, message: impl Into<String>) -> Self {
        Self::new(
            DescriptorBoundLaunchErrorKind::FilesystemIdentity,
            stage,
            message,
        )
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self::new(
            DescriptorBoundLaunchErrorKind::UnsupportedExecutable,
            "executable",
            message,
        )
    }

    fn credentials(message: impl Into<String>) -> Self {
        Self::new(
            DescriptorBoundLaunchErrorKind::Credentials,
            "credentials",
            message,
        )
    }

    fn alias(stage: &'static str, message: impl Into<String>) -> Self {
        Self::new(
            DescriptorBoundLaunchErrorKind::DescriptorAlias,
            stage,
            message,
        )
    }

    fn spawn(message: impl Into<String>) -> Self {
        Self::new(DescriptorBoundLaunchErrorKind::Spawn, "spawn", message)
    }

    fn output_capture(stage: &'static str, message: impl Into<String>) -> Self {
        Self::new(
            DescriptorBoundLaunchErrorKind::OutputCapture,
            stage,
            message,
        )
    }

    fn output_limit(stage: &'static str) -> Self {
        Self::new(
            DescriptorBoundLaunchErrorKind::OutputLimit,
            stage,
            format!("child {stage} exceeded the {MAX_CAPTURED_STREAM_BYTES}-byte capture limit"),
        )
    }

    fn status(message: impl Into<String>) -> Self {
        Self::new(DescriptorBoundLaunchErrorKind::Status, "status", message)
    }
}

impl fmt::Debug for DescriptorBoundLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DescriptorBoundLaunchError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("public_message", &self.public_message)
            .finish()
    }
}

impl fmt::Display for DescriptorBoundLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for DescriptorBoundLaunchError {}

trait LaunchHooks {
    fn after_descriptors_opened(&self) -> Result<(), DescriptorBoundLaunchError> {
        Ok(())
    }

    fn after_spawn(&self) -> Result<(), DescriptorBoundLaunchError> {
        Ok(())
    }
}

fn validate_command_id(value: &str) -> Result<(), DescriptorBoundLaunchError> {
    if value.is_empty()
        || value.len() > MAX_COMMAND_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(DescriptorBoundLaunchError::plan(
            "command_id",
            "command ID must use bounded ASCII letters, digits, '.', '_', or '-'",
        ));
    }
    Ok(())
}

fn validate_absolute_path(
    stage: &'static str,
    path: PathBuf,
) -> Result<PathBuf, DescriptorBoundLaunchError> {
    let Some(value) = path.to_str() else {
        return Err(DescriptorBoundLaunchError::plan(
            stage,
            "reviewed path must be valid UTF-8",
        ));
    };
    if value.is_empty()
        || value == "/"
        || value.len() > MAX_PATH_BYTES
        || value.ends_with('/')
        || value.contains("//")
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(DescriptorBoundLaunchError::plan(
            stage,
            "reviewed path must be a canonical non-root absolute path",
        ));
    }
    Ok(path)
}

fn normal_components(path: &Path) -> Result<Vec<&OsStr>, DescriptorBoundLaunchError> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            Component::RootDir => None,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(DescriptorBoundLaunchError::plan(
            "path",
            "reviewed path has no normal component",
        ));
    }
    Ok(components)
}

fn validate_values(
    stage: &'static str,
    values: &[ReviewedLaunchValue],
    maximum_count: usize,
) -> Result<(), DescriptorBoundLaunchError> {
    if values.len() > maximum_count {
        return Err(DescriptorBoundLaunchError::plan(
            stage,
            format!("reviewed {stage} exceed the fixed count limit"),
        ));
    }
    let mut total = 0_usize;
    for value in values {
        let exposed = value.exposed();
        if exposed.len() > MAX_VALUE_BYTES || exposed.as_bytes().contains(&0) {
            return Err(DescriptorBoundLaunchError::plan(
                stage,
                format!("reviewed {stage} contain an invalid or oversized value"),
            ));
        }
        total = total.checked_add(exposed.len()).ok_or_else(|| {
            DescriptorBoundLaunchError::plan(
                stage,
                format!("reviewed {stage} exceed the fixed total-size limit"),
            )
        })?;
    }
    if total > MAX_TOTAL_VALUE_BYTES {
        return Err(DescriptorBoundLaunchError::plan(
            stage,
            format!("reviewed {stage} exceed the fixed total-size limit"),
        ));
    }
    Ok(())
}

fn validate_environment(
    environment: &BTreeMap<String, ReviewedLaunchValue>,
) -> Result<(), DescriptorBoundLaunchError> {
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(DescriptorBoundLaunchError::plan(
            "environment",
            "reviewed environment exceeds the fixed entry limit",
        ));
    }
    let values = environment.values().cloned().collect::<Vec<_>>();
    validate_values("environment", &values, MAX_ENVIRONMENT_ENTRIES)?;
    for key in environment.keys() {
        if key.is_empty()
            || key.len() > 128
            || key.as_bytes().contains(&0)
            || key.contains('=')
            || !key
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            return Err(DescriptorBoundLaunchError::plan(
                "environment",
                "reviewed environment contains an invalid variable name",
            ));
        }
    }
    Ok(())
}

mod executor;

/// Execute one exact descriptor-bound Linux launch.
///
/// Privilege-dropping launches run on a short-lived helper thread whose supplementary
/// groups are cleared before the child is spawned. This makes group clearing fail-closed
/// without changing the caller thread's credentials or introducing unsafe Rust.
pub fn execute_reviewed_linux_launch(
    plan: &ReviewedLinuxLaunchPlan,
) -> Result<DescriptorBoundLaunchReceipt, DescriptorBoundLaunchError> {
    match plan.credentials() {
        ReviewedLaunchCredentials::Inherit { .. } => executor::execute_reviewed_linux_launch(plan),
        ReviewedLaunchCredentials::DropPrivileges { .. } => {
            let plan = plan.clone();
            std::thread::Builder::new()
                .name("smolrunner-credential-launch".to_owned())
                .spawn(move || {
                    rustix::thread::set_thread_groups(&[]).map_err(|_| {
                        DescriptorBoundLaunchError::credentials(
                            "could not clear supplementary groups for the reviewed launch",
                        )
                    })?;
                    executor::execute_reviewed_linux_launch(&plan)
                })
                .map_err(|_| {
                    DescriptorBoundLaunchError::spawn(
                        "could not create the isolated credential launch thread",
                    )
                })?
                .join()
                .map_err(|_| {
                    DescriptorBoundLaunchError::spawn(
                        "the isolated credential launch thread failed",
                    )
                })?
        }
    }
}

#[cfg(test)]
use executor::execute_with_hooks;

#[cfg(test)]
mod tests;
