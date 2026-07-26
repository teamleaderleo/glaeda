//! Typed logical paths for SmolRunner's durable host state.
//!
//! This module performs no filesystem access. Linux persistence must open the trusted state root
//! and traverse these validated components one directory descriptor at a time.

use std::fmt;

use serde::Serialize;

pub const STATE_ROOT: &str = "/var/lib/smolrunner";

const MAX_COMPONENT_LEN: usize = 128;
const MAX_RECORD_ID_LEN: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct StateComponent(String);

impl StateComponent {
    /// Validate one relative state-path component.
    ///
    /// # Errors
    ///
    /// Returns an error for empty values, path aliases, separators, non-ASCII values, uppercase
    /// characters, or values longer than the state-component limit.
    pub fn parse(field: &str, value: &str) -> Result<Self, StatePathError> {
        validate_component(field, value, MAX_COMPONENT_LEN)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn fixed(value: &str) -> Self {
        Self(value.to_owned())
    }

    fn json_file(record_id: &Self) -> Self {
        Self(format!("{}.json", record_id.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct InstallationId(StateComponent);

impl InstallationId {
    /// Validate an opaque installation identifier from ADR 0002.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is 16 to 80 lowercase ASCII letters, digits, or `-`.
    pub fn parse(value: &str) -> Result<Self, StatePathError> {
        if !(16..=80).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(StatePathError::new(
                "installation ID",
                "must be 16 to 80 lowercase ASCII letters, digits, or '-'",
            ));
        }

        Ok(Self(StateComponent(value.to_owned())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResourceRecordId(StateComponent);

impl ResourceRecordId {
    /// Validate one resource-record filename stem.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be used as one bounded state-path component.
    pub fn parse(value: &str) -> Result<Self, StatePathError> {
        validate_component("resource record ID", value, MAX_RECORD_ID_LEN)?;
        Ok(Self(StateComponent(value.to_owned())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct JournalId(StateComponent);

impl JournalId {
    /// Validate one execution-journal filename stem.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be used as one bounded state-path component.
    pub fn parse(value: &str) -> Result<Self, StatePathError> {
        validate_component("journal ID", value, MAX_RECORD_ID_LEN)?;
        Ok(Self(StateComponent(value.to_owned())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatePath {
    components: Vec<StateComponent>,
}

impl StatePath {
    #[must_use]
    pub fn components(&self) -> &[StateComponent] {
        &self.components
    }

    fn new(components: Vec<StateComponent>) -> Self {
        Self { components }
    }
}

pub struct StateLayout;

impl StateLayout {
    #[must_use]
    pub fn installation(installation_id: &InstallationId) -> StatePath {
        StatePath::new(vec![
            StateComponent::fixed("installations"),
            installation_id.0.clone(),
        ])
    }

    #[must_use]
    pub fn project_document(installation_id: &InstallationId) -> StatePath {
        let mut components = Self::installation(installation_id).components;
        components.push(StateComponent::fixed("project.json"));
        StatePath::new(components)
    }

    #[must_use]
    pub fn resource_document(
        installation_id: &InstallationId,
        resource_id: &ResourceRecordId,
    ) -> StatePath {
        let mut components = Self::installation(installation_id).components;
        components.push(StateComponent::fixed("resources"));
        components.push(StateComponent::json_file(&resource_id.0));
        StatePath::new(components)
    }

    #[must_use]
    pub fn journal_document(installation_id: &InstallationId, journal_id: &JournalId) -> StatePath {
        let mut components = Self::installation(installation_id).components;
        components.push(StateComponent::fixed("journals"));
        components.push(StateComponent::json_file(&journal_id.0));
        StatePath::new(components)
    }

    #[must_use]
    pub fn execution_receipt_document(
        installation_id: &InstallationId,
        execution_id: &JournalId,
    ) -> StatePath {
        let mut components = Self::installation(installation_id).components;
        components.push(StateComponent::fixed("receipts"));
        components.push(StateComponent::json_file(&execution_id.0));
        StatePath::new(components)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatePathError {
    pub field: String,
    pub problem: String,
}

impl StatePathError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for StatePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for StatePathError {}

fn validate_component(field: &str, value: &str, max_len: usize) -> Result<(), StatePathError> {
    let mut bytes = value.bytes();
    let first_is_safe = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let remaining_are_safe = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });

    if value == "."
        || value == ".."
        || value.len() > max_len
        || !first_is_safe
        || !remaining_are_safe
    {
        return Err(StatePathError::new(
            field,
            format!(
                "must be one relative component of at most {max_len} lowercase ASCII letters, digits, '.', '_', or '-', beginning with a letter or digit"
            ),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        InstallationId, JournalId, ResourceRecordId, STATE_ROOT, StateComponent, StateLayout,
    };

    fn component_strings(path: &super::StatePath) -> Vec<&str> {
        path.components()
            .iter()
            .map(StateComponent::as_str)
            .collect()
    }

    #[test]
    fn state_root_matches_the_accepted_layout() {
        assert_eq!(STATE_ROOT, "/var/lib/smolrunner");
    }

    #[test]
    fn installation_id_uses_the_accepted_opaque_format() {
        let id = InstallationId::parse("0123456789abcdef").expect("valid installation ID");
        assert_eq!(id.as_str(), "0123456789abcdef");

        for invalid in [
            "short",
            "UPPERCASE0123456",
            "0123456789abc_def",
            "0123456789abc.def",
            "0123456789abc/def",
        ] {
            InstallationId::parse(invalid).expect_err("unsafe installation ID must fail");
        }
    }

    #[test]
    fn untrusted_components_cannot_smuggle_path_syntax() {
        for invalid in [
            "",
            ".",
            "..",
            "/absolute",
            "parent/child",
            "parent\\child",
            " leading",
            "Uppercase",
            "line\nbreak",
        ] {
            StateComponent::parse("test component", invalid)
                .expect_err("unsafe path component must fail");
        }
    }

    #[test]
    fn layout_is_expressed_as_single_validated_components() {
        let installation =
            InstallationId::parse("0123456789abcdef").expect("valid installation ID");
        let resource =
            ResourceRecordId::parse("linux-user-project-runner").expect("valid resource record ID");
        let journal = JournalId::parse("apply-00000001").expect("valid journal ID");

        assert_eq!(
            component_strings(&StateLayout::installation(&installation)),
            ["installations", "0123456789abcdef"]
        );
        assert_eq!(
            component_strings(&StateLayout::project_document(&installation)),
            ["installations", "0123456789abcdef", "project.json"]
        );
        assert_eq!(
            component_strings(&StateLayout::resource_document(&installation, &resource)),
            [
                "installations",
                "0123456789abcdef",
                "resources",
                "linux-user-project-runner.json"
            ]
        );
        assert_eq!(
            component_strings(&StateLayout::journal_document(&installation, &journal)),
            [
                "installations",
                "0123456789abcdef",
                "journals",
                "apply-00000001.json"
            ]
        );
        assert_eq!(
            component_strings(&StateLayout::execution_receipt_document(
                &installation,
                &journal,
            )),
            [
                "installations",
                "0123456789abcdef",
                "receipts",
                "apply-00000001.json"
            ]
        );
    }

    #[test]
    fn record_ids_are_bounded_and_lowercase() {
        let too_long = "a".repeat(101);
        ResourceRecordId::parse(&too_long).expect_err("oversized resource ID must fail");
        JournalId::parse("Apply-1").expect_err("uppercase journal ID must fail");
    }
}
