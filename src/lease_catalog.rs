use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::lease::{LeaseAction, LeaseId, LeaseIdentity, LeaseRecord};
use crate::state::InstallationId;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct LeaseSelector {
    pub installation_id: InstallationId,
    pub lease_id: LeaseId,
}

impl LeaseSelector {
    #[must_use]
    pub const fn new(installation_id: InstallationId, lease_id: LeaseId) -> Self {
        Self {
            installation_id,
            lease_id,
        }
    }

    #[must_use]
    pub fn from_identity(identity: &LeaseIdentity) -> Self {
        Self::new(identity.installation_id.clone(), identity.lease_id.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseWriteDisposition {
    Created,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseWriteReceipt {
    pub disposition: LeaseWriteDisposition,
    pub revision: u64,
}

impl LeaseWriteReceipt {
    #[must_use]
    pub const fn new(disposition: LeaseWriteDisposition, revision: u64) -> Self {
        Self {
            disposition,
            revision,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStoreErrorKind {
    AlreadyExists,
    Missing,
    Conflict,
    Busy,
    InvalidTransition,
    CorruptState,
    UnsafeFilesystem,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseStoreError {
    pub kind: LeaseStoreErrorKind,
    pub message: String,
}

impl LeaseStoreError {
    #[must_use]
    pub fn public(kind: LeaseStoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for LeaseStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for LeaseStoreError {}

/// Atomic persistence boundary for lease records.
///
/// Implementations must make `create` a no-replace operation and must make
/// `replace_if_revision` compare and publish while holding one exclusive write boundary. A
/// read-then-write implementation without a lock or transactional primitive violates this contract.
pub trait LeaseStore {
    fn load(&self, selector: &LeaseSelector) -> Result<Option<LeaseRecord>, LeaseStoreError>;

    fn create(&mut self, record: &LeaseRecord) -> Result<LeaseWriteReceipt, LeaseStoreError>;

    fn replace_if_revision(
        &mut self,
        expected_revision: u64,
        record: &LeaseRecord,
    ) -> Result<LeaseWriteReceipt, LeaseStoreError>;
}

#[derive(Debug)]
pub struct LeaseCatalog<S> {
    store: S,
}

impl<S> LeaseCatalog<S> {
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn into_store(self) -> S {
        self.store
    }
}

impl<S: LeaseStore> LeaseCatalog<S> {
    /// Create one pending lease with atomic no-replace semantics.
    ///
    /// # Errors
    ///
    /// Returns an error when the selector already exists or the store cannot publish the record.
    pub fn create(
        &mut self,
        identity: LeaseIdentity,
    ) -> Result<(LeaseRecord, LeaseWriteReceipt), LeaseStoreError> {
        let record = LeaseRecord::pending(identity);
        let receipt = self.store.create(&record)?;
        Ok((record, receipt))
    }

    /// Load one lease by installation and lease identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded store error for corrupt or inaccessible persisted state.
    pub fn load(&self, selector: &LeaseSelector) -> Result<Option<LeaseRecord>, LeaseStoreError> {
        self.store.load(selector)
    }

    /// Plan and atomically publish one revision-checked lease transition.
    ///
    /// # Errors
    ///
    /// Returns `Missing` when the lease is absent, `Conflict` when the caller's revision is stale,
    /// `InvalidTransition` when the requested action is illegal, and `CorruptState` when the
    /// persisted identity is inconsistent.
    pub fn transition(
        &mut self,
        selector: &LeaseSelector,
        expected_revision: u64,
        action: LeaseAction,
    ) -> Result<(LeaseRecord, LeaseWriteReceipt), LeaseStoreError> {
        let current = self.store.load(selector)?.ok_or_else(|| {
            LeaseStoreError::public(LeaseStoreErrorKind::Missing, "lease does not exist")
        })?;
        if LeaseSelector::from_identity(&current.identity) != *selector {
            return Err(LeaseStoreError::public(
                LeaseStoreErrorKind::CorruptState,
                "persisted lease identity does not match its selector",
            ));
        }
        if current.revision != expected_revision {
            return Err(stale_revision(expected_revision, current.revision));
        }
        let transition = current.plan_transition(action).map_err(|error| {
            LeaseStoreError::public(LeaseStoreErrorKind::InvalidTransition, error.to_string())
        })?;
        let next = transition.resulting_record();
        let receipt = self.store.replace_if_revision(expected_revision, &next)?;
        Ok((next, receipt))
    }
}

/// Deterministic in-memory implementation of the atomic lease-store contract.
///
/// This implementation is useful for tests, local simulations, and callers that already provide a
/// durable outer transaction. It does not claim process durability.
#[derive(Debug, Default)]
pub struct MemoryLeaseStore {
    records: BTreeMap<LeaseSelector, LeaseRecord>,
}

impl MemoryLeaseStore {
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl LeaseStore for MemoryLeaseStore {
    fn load(&self, selector: &LeaseSelector) -> Result<Option<LeaseRecord>, LeaseStoreError> {
        Ok(self.records.get(selector).cloned())
    }

    fn create(&mut self, record: &LeaseRecord) -> Result<LeaseWriteReceipt, LeaseStoreError> {
        let selector = LeaseSelector::from_identity(&record.identity);
        if self.records.contains_key(&selector) {
            return Err(LeaseStoreError::public(
                LeaseStoreErrorKind::AlreadyExists,
                "lease already exists",
            ));
        }
        self.records.insert(selector, record.clone());
        Ok(LeaseWriteReceipt::new(
            LeaseWriteDisposition::Created,
            record.revision,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: u64,
        record: &LeaseRecord,
    ) -> Result<LeaseWriteReceipt, LeaseStoreError> {
        let selector = LeaseSelector::from_identity(&record.identity);
        let current = self.records.get(&selector).ok_or_else(|| {
            LeaseStoreError::public(LeaseStoreErrorKind::Missing, "lease does not exist")
        })?;
        if current.identity != record.identity {
            return Err(LeaseStoreError::public(
                LeaseStoreErrorKind::CorruptState,
                "replacement lease identity differs from persisted identity",
            ));
        }
        if current.revision != expected_revision {
            return Err(stale_revision(expected_revision, current.revision));
        }
        let Some(next_revision) = expected_revision.checked_add(1) else {
            return Err(LeaseStoreError::public(
                LeaseStoreErrorKind::Conflict,
                "lease revision counter is exhausted",
            ));
        };
        if record.revision != next_revision {
            return Err(LeaseStoreError::public(
                LeaseStoreErrorKind::Conflict,
                "replacement revision must advance exactly once",
            ));
        }
        self.records.insert(selector, record.clone());
        Ok(LeaseWriteReceipt::new(
            LeaseWriteDisposition::Replaced,
            record.revision,
        ))
    }
}

fn stale_revision(expected: u64, actual: u64) -> LeaseStoreError {
    LeaseStoreError::public(
        LeaseStoreErrorKind::Conflict,
        format!("stale lease revision: expected {expected}, current revision is {actual}"),
    )
}

#[cfg(test)]
mod tests {
    use crate::lease::{LeaseAction, LeaseId, LeaseIdentity, LeaseKind, LeaseRecord, LeaseState};
    use crate::state::InstallationId;

    use super::{
        LeaseCatalog, LeaseSelector, LeaseStore, LeaseStoreErrorKind, LeaseWriteDisposition,
        MemoryLeaseStore,
    };

    fn identity() -> LeaseIdentity {
        LeaseIdentity::new(
            LeaseId::parse("preview-pr-42").expect("lease ID"),
            InstallationId::parse("installation-001").expect("installation ID"),
            LeaseKind::Preview,
        )
    }

    #[test]
    fn create_is_atomic_no_replace() {
        let mut catalog = LeaseCatalog::new(MemoryLeaseStore::default());
        let (_, receipt) = catalog.create(identity()).expect("create lease");
        assert_eq!(receipt.disposition, LeaseWriteDisposition::Created);

        let error = catalog.create(identity()).expect_err("duplicate must fail");
        assert_eq!(error.kind, LeaseStoreErrorKind::AlreadyExists);
    }

    #[test]
    fn transition_publishes_exactly_one_revision() {
        let mut catalog = LeaseCatalog::new(MemoryLeaseStore::default());
        let (pending, _) = catalog.create(identity()).expect("create lease");
        let selector = LeaseSelector::from_identity(&pending.identity);
        let (active, receipt) = catalog
            .transition(&selector, 0, LeaseAction::Activate)
            .expect("activate lease");

        assert_eq!(active.state, LeaseState::Active);
        assert_eq!(active.revision, 1);
        assert_eq!(receipt.disposition, LeaseWriteDisposition::Replaced);
        assert_eq!(receipt.revision, 1);
    }

    #[test]
    fn stale_revision_is_rejected_after_another_transition() {
        let mut catalog = LeaseCatalog::new(MemoryLeaseStore::default());
        let (pending, _) = catalog.create(identity()).expect("create lease");
        let selector = LeaseSelector::from_identity(&pending.identity);
        catalog
            .transition(&selector, 0, LeaseAction::Activate)
            .expect("activate lease");

        let error = catalog
            .transition(&selector, 0, LeaseAction::Expire)
            .expect_err("stale transition must fail");
        assert_eq!(error.kind, LeaseStoreErrorKind::Conflict);
        assert!(error.message.contains("current revision is 1"));
    }

    #[test]
    fn invalid_lifecycle_transition_leaves_persisted_state_unchanged() {
        let mut catalog = LeaseCatalog::new(MemoryLeaseStore::default());
        let (pending, _) = catalog.create(identity()).expect("create lease");
        let selector = LeaseSelector::from_identity(&pending.identity);

        let error = catalog
            .transition(&selector, 0, LeaseAction::Wake)
            .expect_err("pending lease cannot wake");
        assert_eq!(error.kind, LeaseStoreErrorKind::InvalidTransition);
        assert_eq!(
            catalog
                .load(&selector)
                .expect("load lease")
                .expect("present lease"),
            pending
        );
    }

    #[test]
    fn revision_exhaustion_is_rejected_by_the_store_contract() {
        let mut store = MemoryLeaseStore::default();
        let mut current = LeaseRecord::pending(identity());
        current.revision = u64::MAX;
        store.create(&current).expect("seed exhausted lease");

        let error = store
            .replace_if_revision(u64::MAX, &current)
            .expect_err("exhausted revision must fail");
        assert_eq!(error.kind, LeaseStoreErrorKind::Conflict);
        assert!(error.message.contains("exhausted"));
    }

    #[test]
    fn missing_lease_is_reported_without_creation() {
        let mut catalog = LeaseCatalog::new(MemoryLeaseStore::default());
        let selector = LeaseSelector::from_identity(&identity());

        let error = catalog
            .transition(&selector, 0, LeaseAction::Activate)
            .expect_err("missing lease must fail");
        assert_eq!(error.kind, LeaseStoreErrorKind::Missing);
        assert!(catalog.into_store().is_empty());
    }
}
