use std::fmt;

use serde::Serialize;

use crate::journal_document::{
    JournalDocumentError, JournalStateDocument, encode_journal_document,
};
use crate::state::{InstallationId, ResourceRecordId, StateLayout, StatePath};
use crate::state_document::{
    ProjectStateDocument, ResourceStateDocument, StateDocument, StateDocumentError,
    encode_state_document,
};

pub const MAX_STATE_DOCUMENT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateRecordKind {
    Project,
    Resource,
    Journal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateRecord {
    kind: StateRecordKind,
    path: StatePath,
    bytes: Vec<u8>,
}

impl StateRecord {
    /// Bind one validated project document to its canonical project-state path.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding fails or the encoded document exceeds the store limit.
    pub fn project(document: ProjectStateDocument) -> Result<Self, StateRecordError> {
        let path = StateLayout::project_document(document.installation_id());
        let encoded = encode_state_document(&StateDocument::Project(document))?;
        Self::finish(StateRecordKind::Project, path, encoded)
    }

    /// Bind one validated resource document to its canonical resource-state path.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker installation identity is invalid, encoding fails, or the
    /// encoded document exceeds the store limit.
    pub fn resource(
        resource_id: &ResourceRecordId,
        document: ResourceStateDocument,
    ) -> Result<Self, StateRecordError> {
        let installation_id = InstallationId::parse(&document.marker().installation_id)
            .map_err(|error| StateRecordError::single(error.to_string()))?;
        let path = StateLayout::resource_document(&installation_id, resource_id);
        let encoded = encode_state_document(&StateDocument::Resource(document))?;
        Self::finish(StateRecordKind::Resource, path, encoded)
    }

    /// Bind one validated journal document to its canonical journal-state path.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding fails or the encoded document exceeds the store limit.
    pub fn journal(document: JournalStateDocument) -> Result<Self, StateRecordError> {
        let path = StateLayout::journal_document(document.installation_id(), document.journal_id());
        let encoded = encode_journal_document(&document)?;
        Self::finish(StateRecordKind::Journal, path, encoded)
    }

    #[must_use]
    pub fn kind(&self) -> StateRecordKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &StatePath {
        &self.path
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn finish(
        kind: StateRecordKind,
        path: StatePath,
        encoded: String,
    ) -> Result<Self, StateRecordError> {
        if encoded.len() > MAX_STATE_DOCUMENT_BYTES {
            return Err(StateRecordError::single(format!(
                "encoded state document exceeds {MAX_STATE_DOCUMENT_BYTES} bytes"
            )));
        }
        Ok(Self {
            kind,
            path,
            bytes: encoded.into_bytes(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateRecordError {
    pub problems: Vec<String>,
}

impl StateRecordError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl From<StateDocumentError> for StateRecordError {
    fn from(error: StateDocumentError) -> Self {
        Self {
            problems: error.problems,
        }
    }
}

impl From<JournalDocumentError> for StateRecordError {
    fn from(error: JournalDocumentError) -> Self {
        Self {
            problems: error.problems,
        }
    }
}

impl fmt::Display for StateRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "state record construction failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for StateRecordError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateRead {
    Missing,
    Present(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateWriteDisposition {
    Created,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateWriteReceipt {
    disposition: StateWriteDisposition,
    bytes_written: usize,
}

impl StateWriteReceipt {
    #[must_use]
    pub fn disposition(&self) -> StateWriteDisposition {
        self.disposition
    }

    #[must_use]
    pub fn bytes_written(&self) -> usize {
        self.bytes_written
    }

    pub const fn new(disposition: StateWriteDisposition, bytes_written: usize) -> Self {
        Self {
            disposition,
            bytes_written,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateStoreErrorKind {
    Busy,
    Conflict,
    Io,
    UnsafeFilesystem,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateStoreError {
    kind: StateStoreErrorKind,
    public_message: String,
}

impl StateStoreError {
    #[must_use]
    pub fn kind(&self) -> StateStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    pub fn public(kind: StateStoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for StateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for StateStoreError {}

/// Narrow persistence boundary for canonical state paths and prevalidated document bytes.
pub trait StateStore {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError>;

    /// Atomically create one record without replacing an existing destination.
    fn create_atomic(&mut self, record: &StateRecord)
    -> Result<StateWriteReceipt, StateStoreError>;

    /// Atomically create or replace one record.
    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError>;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::journal::{
        ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
        PlannedMutation, Preconditions, RollbackClass,
    };
    use crate::journal_document::JournalStateDocument;
    use crate::manifest::RunnerScope;
    use crate::ownership::{OwnershipMarker, ProjectIdentity, ResourceIdentity};
    use crate::resource::AccountPolicy;
    use crate::state::{InstallationId, JournalId, ResourceRecordId, StateComponent};
    use crate::state_document::{ProjectStateDocument, ResourceStateDocument};

    use super::{
        StateRead, StateRecord, StateRecordKind, StateStore, StateStoreError,
        StateWriteDisposition, StateWriteReceipt,
    };

    #[derive(Default)]
    struct MemoryStore {
        entries: BTreeMap<Vec<String>, Vec<u8>>,
    }

    impl StateStore for MemoryStore {
        fn read(&self, path: &crate::state::StatePath) -> Result<StateRead, StateStoreError> {
            let key = path
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            Ok(self
                .entries
                .get(&key)
                .cloned()
                .map_or(StateRead::Missing, StateRead::Present))
        }

        fn create_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            match self.entries.entry(key) {
                std::collections::btree_map::Entry::Occupied(_) => Err(StateStoreError::public(
                    super::StateStoreErrorKind::Conflict,
                    "state destination already exists",
                )),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(record.bytes().to_vec());
                    Ok(StateWriteReceipt::new(
                        StateWriteDisposition::Created,
                        record.bytes().len(),
                    ))
                }
            }
        }

        fn write_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let disposition = if self.entries.insert(key, record.bytes().to_vec()).is_some() {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            };
            Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
        }
    }

    fn project() -> ProjectIdentity {
        ProjectIdentity {
            repository: "example/project".to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "project-runner".to_owned(),
        }
    }

    fn installation_id() -> InstallationId {
        InstallationId::parse("0123456789abcdef").expect("installation ID")
    }

    #[test]
    fn project_record_binds_validated_bytes_to_the_canonical_path() {
        let record = StateRecord::project(
            ProjectStateDocument::new(installation_id(), project()).expect("project document"),
        )
        .expect("project record");
        assert_eq!(record.kind(), StateRecordKind::Project);
        assert_eq!(
            record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .collect::<Vec<_>>(),
            ["installations", "0123456789abcdef", "project.json"]
        );
        assert!(record.bytes().ends_with(b"\n"));
    }

    #[test]
    fn resource_and_journal_records_derive_their_installation_paths() {
        let marker = OwnershipMarker::new(
            "0123456789abcdef",
            project(),
            ResourceIdentity::linux_user(
                "project-runner",
                1001,
                1001,
                "/var/lib/project-runner",
                "/usr/sbin/nologin",
                AccountPolicy::Service,
            )
            .expect("resource identity"),
        );
        let resource = StateRecord::resource(
            &ResourceRecordId::parse("linux-user-project-runner").expect("resource ID"),
            ResourceStateDocument::new(marker).expect("resource document"),
        )
        .expect("resource record");
        assert_eq!(resource.kind(), StateRecordKind::Resource);

        let action = PlannedMutation::new(
            "one",
            ExecutionLane::Root,
            "perform one",
            RollbackClass::Reversible,
            Preconditions::new(["observed state"]),
        );
        let journal = ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![JournalRecord {
                action,
                outcome: ActionOutcome::Completed,
                message: Some("completed one".to_owned()),
            }],
            stopped_after: None,
        };
        let journal = StateRecord::journal(
            JournalStateDocument::new(
                installation_id(),
                JournalId::parse("apply-00000001").expect("journal ID"),
                journal,
            )
            .expect("journal document"),
        )
        .expect("journal record");
        assert_eq!(journal.kind(), StateRecordKind::Journal);
    }

    #[test]
    fn store_contract_receives_only_bound_records() {
        let record = StateRecord::project(
            ProjectStateDocument::new(installation_id(), project()).expect("project document"),
        )
        .expect("project record");
        let mut store = MemoryStore::default();
        let first = store.create_atomic(&record).expect("first create");
        assert_eq!(first.disposition(), StateWriteDisposition::Created);
        assert_eq!(first.bytes_written(), record.bytes().len());
        let conflict = store
            .create_atomic(&record)
            .expect_err("create-only publication must not replace");
        assert_eq!(conflict.kind(), super::StateStoreErrorKind::Conflict);
        let second = store.write_atomic(&record).expect("replacement write");
        assert_eq!(second.disposition(), StateWriteDisposition::Replaced);
        assert_eq!(
            store.read(record.path()).expect("read state"),
            StateRead::Present(record.bytes().to_vec())
        );
    }
}
