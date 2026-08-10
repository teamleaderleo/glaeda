use std::fmt;

use serde::Serialize;

pub const GITHUB_SCALE_SET_PROTOCOL_SCHEMA_VERSION: u8 = 1;
const MAX_RUNNER_NAME_LEN: usize = 96;
const MAX_JOB_ID_LEN: usize = 256;
const MAX_JOB_RESULT_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ScaleSetRunnerName(String);

impl ScaleSetRunnerName {
    /// Parse one SmolRunner-owned stable runner name chosen before JIT creation.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, non-ASCII, or non-canonical name.
    pub fn parse(value: &str) -> Result<Self, GitHubScaleSetProtocolError> {
        if value.is_empty()
            || value.len() > MAX_RUNNER_NAME_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || value.starts_with('-')
            || value.ends_with('-')
        {
            return Err(GitHubScaleSetProtocolError::new(
                "runner_name",
                "invalid_runner_name",
                "runner name must be bounded lowercase ASCII with interior hyphens only",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ScaleSetRunnerId(u64);

impl ScaleSetRunnerId {
    /// Construct one positive GitHub service-assigned runner ID.
    ///
    /// # Errors
    ///
    /// Returns an error for zero.
    pub fn new(value: u64) -> Result<Self, GitHubScaleSetProtocolError> {
        if value == 0 {
            return Err(GitHubScaleSetProtocolError::new(
                "runner_id",
                "invalid_runner_id",
                "runner ID must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ScaleSetJobId(String);

impl ScaleSetJobId {
    /// Parse the Scale Set wire job ID as an opaque bounded value.
    ///
    /// GitHub's Scale Set client models this field as a string. SmolRunner compares the exact
    /// value and does not narrow it to the numeric REST workflow-job identity.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, whitespace-bearing, or control-bearing value.
    pub fn parse(value: &str) -> Result<Self, GitHubScaleSetProtocolError> {
        if value.is_empty()
            || value.len() > MAX_JOB_ID_LEN
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(GitHubScaleSetProtocolError::new(
                "job_id",
                "invalid_job_id",
                "Scale Set job ID must be bounded non-whitespace printable ASCII",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ScaleSetJobResult(String);

impl ScaleSetJobResult {
    /// Parse one bounded completion result without inventing a closed service enum.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, non-ASCII, control-bearing, or edge-spaced
    /// value.
    pub fn parse(value: &str) -> Result<Self, GitHubScaleSetProtocolError> {
        if value.is_empty()
            || value.len() > MAX_JOB_RESULT_LEN
            || value.trim() != value
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(GitHubScaleSetProtocolError::new(
                "job_result",
                "invalid_job_result",
                "Scale Set job result must be bounded printable ASCII without edge whitespace",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaleSetRunnerReference {
    pub id: ScaleSetRunnerId,
    pub name: ScaleSetRunnerName,
}

impl ScaleSetRunnerReference {
    #[must_use]
    pub fn new(id: ScaleSetRunnerId, name: ScaleSetRunnerName) -> Self {
        Self { id, name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ScaleSetJobEvent {
    Started {
        runner: ScaleSetRunnerReference,
        job_id: ScaleSetJobId,
    },
    Completed {
        #[serde(skip_serializing_if = "Option::is_none")]
        runner: Option<ScaleSetRunnerReference>,
        job_id: ScaleSetJobId,
        result: ScaleSetJobResult,
    },
}

impl ScaleSetJobEvent {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        GITHUB_SCALE_SET_PROTOCOL_SCHEMA_VERSION
    }

    #[must_use]
    pub fn runner(&self) -> Option<&ScaleSetRunnerReference> {
        match self {
            Self::Started { runner, .. } => Some(runner),
            Self::Completed { runner, .. } => runner.as_ref(),
        }
    }

    #[must_use]
    pub fn job_id(&self) -> &ScaleSetJobId {
        match self {
            Self::Started { job_id, .. } | Self::Completed { job_id, .. } => job_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubScaleSetProtocolError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl GitHubScaleSetProtocolError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for GitHubScaleSetProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for GitHubScaleSetProtocolError {}
