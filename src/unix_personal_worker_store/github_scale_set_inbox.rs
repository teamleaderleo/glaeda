// The store is private until the M3 consumer owns poll/apply/ack sequencing. Remove this allowance
// when that service calls the recovery and replacement entry points in production.
#![allow(dead_code)]

use super::*;

use crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument;
use crate::github_scale_set_bridge::ScaleSetBridgeIdentity;
use crate::github_scale_set_bridge::ScaleSetBridgePoll;
use crate::github_scale_set_inbox::{
    MAX_GITHUB_SCALE_SET_INBOX_BYTES, PendingScaleSetMessage, ScaleSetAckReceipt,
    ScaleSetInboxDocument, ScaleSetInboxError, ScaleSetInboxRevision, decode_scale_set_inbox,
    encode_scale_set_inbox,
};

const INBOX_DOCUMENT: &str = "github-scale-set-inbox.json";
const STAGED_INBOX_DOCUMENT: &str = ".github-scale-set-inbox.next.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlan {
    Clean,
    PublishStaged { no_replace: bool },
    RemoveStaleStaged,
}

impl UnixPersonalWorkerStore {
    /// Open the shared store specifically to recover Scale Set inbox publication debt.
    pub(crate) fn open_or_recover_scale_set_inbox(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, ScaleSetInboxError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(|error| map_store_error(map_root_open_error(error)))?;
        let root_stat = inspect_directory(&root, "Scale Set inbox state root", None)
            .map_err(map_store_error)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let (directory, publication_lock) =
            open_or_publish_initialization_directory(&root, owner).map_err(map_store_error)?;
        let mut store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = match publication_lock {
            Some(lock) => lock,
            None => store.acquire_mutation_lock().map_err(map_store_error)?,
        };
        synchronize_directory(&store._root, "Scale Set inbox state root")
            .map_err(map_store_error)?;
        synchronize_directory(&store.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_other_stages_clean(&store)?;
        store.recover_scale_set_inbox_locked()?;
        Ok(store)
    }

    /// Initialize or recover the private persistence-before-ack Scale Set inbox.
    pub(crate) fn initialize_scale_set_inbox(
        &mut self,
        source_identity: &ScaleSetBridgeIdentity,
    ) -> Result<ScaleSetInboxDocument, ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_other_stages_clean(self)?;
        self.recover_scale_set_inbox_locked()?;
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        if let Some(current) = self.load_scale_set_inbox_named(INBOX_DOCUMENT)? {
            if current.source_identity() != source_identity {
                return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
            }
            return Ok(current);
        }
        if catalog != DisposableAttemptCatalogDocument::empty() {
            return Err(ScaleSetInboxError::new(
                "inbox_catalog_history_without_inbox",
            ));
        }

        let document = ScaleSetInboxDocument::empty(source_identity.clone());
        let mut staged = self.stage_scale_set_inbox(&document)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, true)
            .map_err(map_store_error)?;
        Ok(document)
    }

    /// Recover an interrupted inbox publication under the shared mutation lock.
    pub(crate) fn recover_scale_set_inbox(&mut self) -> Result<(), ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_other_stages_clean(self)?;
        self.recover_scale_set_inbox_locked()
    }

    /// Load the current inbox only when no recovery publication is pending.
    pub(crate) fn load_scale_set_inbox(
        &self,
    ) -> Result<Option<ScaleSetInboxDocument>, ScaleSetInboxError> {
        let _lock = self.acquire_read_lock().map_err(map_store_error)?;
        if self
            .read_named_bytes_bounded(STAGED_INBOX_DOCUMENT, MAX_GITHUB_SCALE_SET_INBOX_BYTES)
            .map_err(map_store_error)?
            .is_some()
        {
            return Err(ScaleSetInboxError::new("inbox_recovery_required"));
        }
        self.load_scale_set_inbox_named(INBOX_DOCUMENT)
    }

    /// Recover and load the exact inbox/catalog pair used by one coordinator decision.
    pub(crate) fn load_scale_set_control_state(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
    ) -> Result<(ScaleSetInboxDocument, DisposableAttemptCatalogDocument), ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;
        self.recover_scale_set_transaction_stages()?;
        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        Ok((inbox, catalog))
    }

    /// Derive current capacity, perform one bounded bridge poll, and persist its exact response
    /// while retaining the canonical mutation lock.
    ///
    /// Keeping these three operations in one transaction prevents a cooperating catalog writer
    /// from consuming capacity after it was advertised but before an offered job is recorded.
    pub(crate) fn poll_and_record_scale_set<E>(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        poll: impl FnOnce(
            u16,
        ) -> Result<
            (
                ScaleSetBridgePoll,
                crate::execution_admission::EpochMillis,
                crate::execution_admission::EpochMillis,
            ),
            E,
        >,
    ) -> Result<
        Result<
            (
                ScaleSetBridgePoll,
                Option<crate::disposable_worker_reconciler::DisposableAttemptId>,
            ),
            E,
        >,
        ScaleSetInboxError,
    > {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;
        self.recover_scale_set_transaction_stages()?;

        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        if inbox.requires_reconciliation() {
            return Err(ScaleSetInboxError::new("inbox_recovery_required"));
        }
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        let usage = catalog.host_usage().map_err(map_catalog_error)?;
        let available_capacity = u16::from(usage.workers() == 0);
        let (response, observed_at, not_after) = match poll(available_capacity) {
            Ok(response) => response,
            Err(error) => return Ok(Err(error)),
        };

        let attempt_id = match &response {
            ScaleSetBridgePoll::Message {
                message_id,
                statistics: _,
                events,
            } => {
                let next = inbox.record(*message_id, observed_at, not_after, events.clone())?;
                let mut staged = self.stage_scale_set_inbox(&next)?;
                self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
                    .map_err(map_store_error)?;
                None
            }
            ScaleSetBridgePoll::Idle { statistics: _ } => {
                let mut candidates = catalog.active().iter().filter(|reservation| {
                    reservation.attempt().github_job_id().is_some()
                        && matches!(
                            reservation.attempt().phase(),
                            crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
                                | crate::disposable_worker_reconciler::DisposableAttemptPhase::CloneAuthorized
                        )
                });
                let Some(candidate) = candidates.next() else {
                    return Ok(Ok((response, None)));
                };
                if candidates.next().is_some() {
                    return Err(ScaleSetInboxError::new("inbox_capacity_invalid"));
                }
                let attempt_id = candidate.attempt().attempt_id().clone();
                let next =
                    inbox.record_idle(observed_at, not_after, catalog.revision(), candidate)?;
                let mut staged = self.stage_scale_set_inbox(&next)?;
                self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
                    .map_err(map_store_error)?;
                Some(attempt_id)
            }
        };
        Ok(Ok((response, attempt_id)))
    }

    /// Persist a message discovered by the live clone-admission poll while the caller retains the
    /// canonical mutation lock. An idle response never enters this path and cannot be replayed as
    /// mutation authority.
    pub(super) fn persist_clone_scale_set_message_locked(
        &mut self,
        pending: crate::disposable_clone_runtime::PendingCloneScaleSetMessage,
    ) -> Result<u32, ScaleSetInboxError> {
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.source_identity() != &pending.source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        if inbox.requires_reconciliation() {
            return Err(ScaleSetInboxError::new("inbox_recovery_required"));
        }
        let ScaleSetBridgePoll::Message {
            message_id,
            statistics: _,
            events,
        } = pending.response
        else {
            return Err(ScaleSetInboxError::new("inbox_message_refused"));
        };
        let next = inbox.record(message_id, pending.observed_at, pending.not_after, events)?;
        let mut staged = self.stage_scale_set_inbox(&next)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
            .map_err(map_store_error)?;
        Ok(message_id)
    }

    /// Finish one exact pre-clone capacity release after its durable cancellation checkpoint.
    pub(crate) fn complete_scale_set_unprovisioned_attempt(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        attempt_id: &crate::disposable_worker_reconciler::DisposableAttemptId,
    ) -> Result<DisposableAttemptCatalogDocument, ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;
        self.recover_scale_set_transaction_stages()?;
        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        if inbox.requires_reconciliation() {
            return Err(ScaleSetInboxError::new("inbox_recovery_required"));
        }
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_release_refused"))?;
        if reservation.attempt().phase()
            != crate::disposable_worker_reconciler::DisposableAttemptPhase::UnprovisionedReleasing
        {
            return Err(ScaleSetInboxError::new("inbox_release_refused"));
        }
        let next = catalog
            .replace_attempt(
                attempt_id,
                reservation.attempt().revision(),
                crate::disposable_attempt_catalog::DisposableAttemptCatalogAction::CompleteUnprovisioned,
            )
            .map_err(map_catalog_error)?;
        next.validate_successor_of(&catalog)
            .map_err(map_catalog_error)?;
        let mut staged = self.stage_catalog(&next).map_err(map_catalog_error)?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(map_store_error)?;
        Ok(next)
    }

    /// Durably consume an expired, unacquired pre-clone reservation without consulting the source
    /// template.
    ///
    /// Template health may gate creation, but it must never gate return of capacity that has not
    /// crossed the clone-start checkpoint or acquired an external GitHub job. An acquired job is
    /// instead retained until an exact upstream cancellation event releases it.
    pub(crate) fn checkpoint_expired_scale_set_preclone_attempt(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        attempt_id: &crate::disposable_worker_reconciler::DisposableAttemptId,
        now: crate::execution_admission::EpochMillis,
    ) -> Result<bool, ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;
        self.recover_scale_set_transaction_stages()?;
        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        if inbox.requires_reconciliation() {
            return Err(ScaleSetInboxError::new("inbox_recovery_required"));
        }
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_release_refused"))?;
        if !matches!(
            reservation.attempt().phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
                | crate::disposable_worker_reconciler::DisposableAttemptPhase::CloneAuthorized
        ) {
            return Err(ScaleSetInboxError::new("inbox_release_refused"));
        }
        if now <= reservation.attempt().not_after() {
            return Ok(false);
        }
        // A clean inbox plus the exact prebound job identity means the Available offer was
        // durably acknowledged as acquired. The unacquired acknowledgement path removes that
        // authority by moving the attempt to UnprovisionedReleasing before the inbox is clean.
        // Never forget an acquired GitHub job merely because its local start deadline elapsed;
        // the service must keep polling until exact upstream cancellation settles it.
        if reservation.attempt().github_job_id().is_some() {
            return Ok(false);
        }
        let next = catalog
            .replace_attempt(
                attempt_id,
                reservation.attempt().revision(),
                crate::disposable_attempt_catalog::DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
            )
            .map_err(map_catalog_error)?;
        next.validate_successor_of(&catalog)
            .map_err(map_catalog_error)?;
        let mut staged = self.stage_catalog(&next).map_err(map_catalog_error)?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(map_store_error)?;
        Ok(true)
    }

    /// Move one exact complete Scale Set attempt into bounded replay history.
    pub(crate) fn retire_scale_set_complete_attempt(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        attempt_id: &crate::disposable_worker_reconciler::DisposableAttemptId,
    ) -> Result<DisposableAttemptCatalogDocument, ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;
        self.recover_scale_set_transaction_stages()?;
        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        if inbox.requires_reconciliation() {
            return Err(ScaleSetInboxError::new("inbox_recovery_required"));
        }
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_retirement_refused"))?;
        let next = catalog
            .retire_complete(attempt_id, reservation.attempt().revision())
            .map_err(map_catalog_error)?;
        next.validate_successor_of(&catalog)
            .map_err(map_catalog_error)?;
        let mut staged = self.stage_catalog(&next).map_err(map_catalog_error)?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(map_store_error)?;
        Ok(next)
    }

    /// Replace one exact inbox revision with its single legal successor.
    pub(crate) fn replace_scale_set_inbox_if_revision(
        &mut self,
        expected_revision: ScaleSetInboxRevision,
        document: &ScaleSetInboxDocument,
    ) -> Result<(), ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_other_stages_clean(self)?;
        self.recover_scale_set_inbox_locked()?;
        let current = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if current.revision() != expected_revision {
            return Err(ScaleSetInboxError::new("inbox_revision_conflict"));
        }
        document.validate_successor_of(&current)?;
        let mut staged = self.stage_scale_set_inbox(document)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
            .map_err(map_store_error)
    }

    /// Apply the next persisted lifecycle event and advance its cursor under one canonical lock.
    ///
    /// A catalog successor, when needed, is durably published before the inbox cursor. After a
    /// crash, replay therefore sees either the prior catalog or an idempotently advanced catalog;
    /// it can never skip an event whose state effect was not durable.
    pub(crate) fn apply_next_scale_set_event<T, E>(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        expected_inbox_revision: ScaleSetInboxRevision,
        apply: impl FnOnce(
            &crate::github_scale_set_inbox::PendingScaleSetMessage,
            &crate::github_scale_set_bridge::ScaleSetBridgeEvent,
            &DisposableAttemptCatalogDocument,
        ) -> Result<(DisposableAttemptCatalogDocument, T), E>,
    ) -> Result<
        Result<(ScaleSetInboxDocument, DisposableAttemptCatalogDocument, T), E>,
        ScaleSetInboxError,
    > {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;

        self.recover_scale_set_transaction_stages()?;

        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.revision() != expected_inbox_revision {
            return Err(ScaleSetInboxError::new("inbox_revision_conflict"));
        }
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        let pending = inbox
            .pending()
            .filter(|pending| !pending.ack_started())
            .ok_or_else(|| ScaleSetInboxError::new("inbox_event_conflict"))?;
        let event_index = pending.next_event_index();
        let event = pending
            .next_event()
            .cloned()
            .ok_or_else(|| ScaleSetInboxError::new("inbox_event_conflict"))?;
        let message_id = pending.message_id();
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        let (next_catalog, output) = match apply(pending, &event, &catalog) {
            Ok(result) => result,
            Err(error) => return Ok(Err(error)),
        };
        if next_catalog != catalog {
            next_catalog
                .validate_successor_of(&catalog)
                .map_err(map_catalog_error)?;
            let mut staged = self
                .stage_catalog(&next_catalog)
                .map_err(map_catalog_error)?;
            self.publish_named_staged(
                &mut staged,
                super::disposable_attempt_catalog::CATALOG_DOCUMENT,
                false,
            )
            .map_err(map_store_error)?;
        }

        let next_inbox = inbox.mark_next_event_applied(message_id, event_index)?;
        let mut staged = self.stage_scale_set_inbox(&next_inbox)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
            .map_err(map_store_error)?;
        Ok(Ok((next_inbox, next_catalog, output)))
    }

    /// Durably checkpoint acknowledgement before invoking the bridge, then retain its exact
    /// acquired-request receipt before releasing the canonical lock.
    pub(crate) fn acknowledge_scale_set_message<E>(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        expected_inbox_revision: ScaleSetInboxRevision,
        expected_catalog_revision: crate::disposable_attempt_catalog::DisposableAttemptCatalogRevision,
        acknowledge: impl FnOnce(
            u32,
            &PendingScaleSetMessage,
            &DisposableAttemptCatalogDocument,
        ) -> Result<Vec<u64>, E>,
    ) -> Result<Result<ScaleSetInboxDocument, E>, ScaleSetInboxError> {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_other_stages_clean(self)?;
        self.recover_scale_set_inbox_locked()?;
        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.revision() != expected_inbox_revision {
            return Err(ScaleSetInboxError::new("inbox_revision_conflict"));
        }
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        if catalog.revision() != expected_catalog_revision {
            return Err(ScaleSetInboxError::new("inbox_catalog_conflict"));
        }
        let pending = inbox
            .pending()
            .filter(|pending| {
                !pending.ack_started() && pending.next_event_index() == pending.events().len()
            })
            .ok_or_else(|| ScaleSetInboxError::new("inbox_ack_refused"))?;
        let message_id = pending.message_id();
        let started = inbox.begin_ack(message_id)?;
        let mut staged = self.stage_scale_set_inbox(&started)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
            .map_err(map_store_error)?;

        let acquired_request_ids = match acknowledge(message_id, pending, &catalog) {
            Ok(acquired) => acquired,
            Err(error) => return Ok(Err(error)),
        };
        let completed = started.complete_ack(message_id, acquired_request_ids)?;
        let mut staged = self.stage_scale_set_inbox(&completed)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
            .map_err(map_store_error)?;
        Ok(Ok(completed))
    }

    /// Reconcile the exact acquired subset into capacity state before admitting another message.
    ///
    /// The catalog successor is published before the inbox outcome marker. A restart between the
    /// publications replays the exact outcome idempotently and cannot provision an unacquired
    /// offer or forget an acquired one.
    pub(crate) fn apply_scale_set_ack_outcome<T, E>(
        &mut self,
        expected_source_identity: &ScaleSetBridgeIdentity,
        expected_inbox_revision: ScaleSetInboxRevision,
        apply: impl FnOnce(
            &ScaleSetAckReceipt,
            &DisposableAttemptCatalogDocument,
        ) -> Result<(DisposableAttemptCatalogDocument, T), E>,
    ) -> Result<
        Result<(ScaleSetInboxDocument, DisposableAttemptCatalogDocument, T), E>,
        ScaleSetInboxError,
    > {
        let _lock = self.acquire_mutation_lock().map_err(map_store_error)?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(map_store_error)?;
        require_non_message_stages_clean(self)?;

        self.recover_scale_set_transaction_stages()?;

        let inbox = self
            .load_scale_set_inbox_named(INBOX_DOCUMENT)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_missing"))?;
        if inbox.revision() != expected_inbox_revision {
            return Err(ScaleSetInboxError::new("inbox_revision_conflict"));
        }
        if inbox.source_identity() != expected_source_identity {
            return Err(ScaleSetInboxError::new("inbox_source_mismatch"));
        }
        let receipt = inbox
            .last_ack()
            .filter(|receipt| !receipt.outcome_applied())
            .ok_or_else(|| ScaleSetInboxError::new("inbox_ack_outcome_refused"))?;
        let message_id = receipt.message_id();
        let catalog = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| ScaleSetInboxError::new("inbox_catalog_missing"))?;
        let (next_catalog, output) = match apply(receipt, &catalog) {
            Ok(result) => result,
            Err(error) => return Ok(Err(error)),
        };
        if next_catalog != catalog {
            next_catalog
                .validate_successor_of(&catalog)
                .map_err(map_catalog_error)?;
            let mut staged = self
                .stage_catalog(&next_catalog)
                .map_err(map_catalog_error)?;
            self.publish_named_staged(
                &mut staged,
                super::disposable_attempt_catalog::CATALOG_DOCUMENT,
                false,
            )
            .map_err(map_store_error)?;
        }

        let next_inbox = inbox.mark_ack_outcome_applied(message_id)?;
        let mut staged = self.stage_scale_set_inbox(&next_inbox)?;
        self.publish_named_staged(&mut staged, INBOX_DOCUMENT, false)
            .map_err(map_store_error)?;
        Ok(Ok((next_inbox, next_catalog, output)))
    }

    fn load_scale_set_inbox_named(
        &self,
        name: &str,
    ) -> Result<Option<ScaleSetInboxDocument>, ScaleSetInboxError> {
        self.read_named_bytes_bounded(name, MAX_GITHUB_SCALE_SET_INBOX_BYTES)
            .map_err(map_store_error)?
            .map(|bytes| decode_scale_set_inbox(&bytes))
            .transpose()
    }

    fn recover_scale_set_transaction_stages(&mut self) -> Result<(), ScaleSetInboxError> {
        let catalog_stage = self
            .read_named_bytes_bounded(
                super::disposable_attempt_catalog::STAGED_CATALOG_DOCUMENT,
                crate::disposable_attempt_catalog::MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
            )
            .map_err(map_store_error)?
            .is_some();
        let inbox_stage = self
            .read_named_bytes_bounded(STAGED_INBOX_DOCUMENT, MAX_GITHUB_SCALE_SET_INBOX_BYTES)
            .map_err(map_store_error)?
            .is_some();
        if catalog_stage && inbox_stage {
            return Err(ScaleSetInboxError::new(
                "inbox_cross_document_recovery_required",
            ));
        }
        if catalog_stage {
            self.recover_catalog_locked().map_err(map_catalog_error)?;
        }
        if inbox_stage {
            self.recover_scale_set_inbox_locked()?;
        }
        Ok(())
    }

    fn inbox_recovery_plan(&self) -> Result<RecoveryPlan, ScaleSetInboxError> {
        let Some(staged) = self.load_scale_set_inbox_named(STAGED_INBOX_DOCUMENT)? else {
            return Ok(RecoveryPlan::Clean);
        };
        match self.load_scale_set_inbox_named(INBOX_DOCUMENT)? {
            None if staged.is_initial() => Ok(RecoveryPlan::PublishStaged { no_replace: true }),
            None => Err(ScaleSetInboxError::new("inbox_corrupt")),
            Some(current) if staged == current => Ok(RecoveryPlan::RemoveStaleStaged),
            Some(current) => {
                staged.validate_successor_of(&current)?;
                Ok(RecoveryPlan::PublishStaged { no_replace: false })
            }
        }
    }

    fn stage_scale_set_inbox(
        &self,
        document: &ScaleSetInboxDocument,
    ) -> Result<StagedDocument<'_>, ScaleSetInboxError> {
        let encoded = encode_scale_set_inbox(document)?;
        self.stage_named_bytes(STAGED_INBOX_DOCUMENT, &encoded)
            .map_err(map_store_error)
    }

    fn synchronize_existing_inbox_stage(
        &self,
        expected: &ScaleSetInboxDocument,
    ) -> Result<(), ScaleSetInboxError> {
        let file = fs::openat(
            &self.directory,
            STAGED_INBOX_DOCUMENT,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(|_| ScaleSetInboxError::new("inbox_stage_unavailable"))?;
        inspect_private_file(&file, self.owner, "staged Scale Set inbox", None)
            .map_err(map_store_error)?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_GITHUB_SCALE_SET_INBOX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ScaleSetInboxError::new("inbox_stage_unavailable"))?;
        if bytes.len() > MAX_GITHUB_SCALE_SET_INBOX_BYTES
            || decode_scale_set_inbox(&bytes).as_ref() != Ok(expected)
        {
            return Err(ScaleSetInboxError::new("inbox_corrupt"));
        }
        file.sync_all()
            .map_err(|_| ScaleSetInboxError::new("inbox_stage_unavailable"))?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged Scale Set inbox",
            Some(bytes.len()),
        )
        .map_err(map_store_error)
    }

    fn recover_scale_set_inbox_locked(&mut self) -> Result<(), ScaleSetInboxError> {
        match self.inbox_recovery_plan()? {
            RecoveryPlan::Clean => Ok(()),
            RecoveryPlan::PublishStaged { no_replace } => {
                let staged = self
                    .load_scale_set_inbox_named(STAGED_INBOX_DOCUMENT)?
                    .ok_or_else(|| ScaleSetInboxError::new("inbox_corrupt"))?;
                self.synchronize_existing_inbox_stage(&staged)?;
                let mut guard =
                    StagedDocument::existing(self.directory.as_fd(), STAGED_INBOX_DOCUMENT);
                self.publish_named_staged(&mut guard, INBOX_DOCUMENT, no_replace)
                    .map_err(map_store_error)
            }
            RecoveryPlan::RemoveStaleStaged => {
                match fs::unlinkat(&self.directory, STAGED_INBOX_DOCUMENT, AtFlags::empty()) {
                    Ok(()) => {
                        synchronize_directory(&self.directory, "personal worker store directory")
                            .map_err(map_store_error)
                    }
                    Err(Errno::NOENT) => Ok(()),
                    Err(_) => Err(ScaleSetInboxError::new("inbox_stage_unavailable")),
                }
            }
        }
    }
}

pub(super) fn refuse_unsettled(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    if store
        .read_named_bytes_bounded(STAGED_INBOX_DOCUMENT, MAX_GITHUB_SCALE_SET_INBOX_BYTES)?
        .is_some()
    {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "Scale Set inbox recovery must complete before another state mutation",
        ));
    }
    if let Some(bytes) =
        store.read_named_bytes_bounded(INBOX_DOCUMENT, MAX_GITHUB_SCALE_SET_INBOX_BYTES)?
    {
        let inbox = decode_scale_set_inbox(&bytes).map_err(|error| {
            let kind = if error.code() == "inbox_version_incompatible" {
                PersonalWorkerStoreErrorKind::VersionIncompatible
            } else {
                PersonalWorkerStoreErrorKind::CorruptState
            };
            store_error(kind, "Scale Set inbox state is invalid")
        })?;
        if inbox.requires_reconciliation() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set inbox reconciliation must complete before another state mutation",
            ));
        }
    }
    Ok(())
}

pub(super) fn refuse_orphan_current_without_catalog(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    if store
        .read_named_bytes_bounded(INBOX_DOCUMENT, MAX_GITHUB_SCALE_SET_INBOX_BYTES)?
        .is_some()
    {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::CorruptState,
            "Scale Set inbox exists without its disposable-attempt catalog",
        ));
    }
    Ok(())
}

pub(super) fn require_settled_source(
    store: &UnixPersonalWorkerStore,
    expected_source_identity: &str,
) -> Result<(), PersonalWorkerStoreError> {
    refuse_unsettled(store)?;
    let bytes = store
        .read_named_bytes_bounded(INBOX_DOCUMENT, MAX_GITHUB_SCALE_SET_INBOX_BYTES)?
        .ok_or_else(|| {
            store_error(
                PersonalWorkerStoreErrorKind::Missing,
                "Scale Set inbox is required before disposable clone admission",
            )
        })?;
    let inbox = decode_scale_set_inbox(&bytes).map_err(|error| {
        let kind = if error.code() == "inbox_version_incompatible" {
            PersonalWorkerStoreErrorKind::VersionIncompatible
        } else {
            PersonalWorkerStoreErrorKind::CorruptState
        };
        store_error(kind, "Scale Set inbox state is invalid")
    })?;
    if inbox.source_identity().as_str() != expected_source_identity {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::InvalidDocument,
            "Scale Set inbox source does not match clone admission",
        ));
    }
    Ok(())
}

fn require_other_stages_clean(store: &UnixPersonalWorkerStore) -> Result<(), ScaleSetInboxError> {
    store
        .refuse_unsettled_personal_worker_state()
        .map_err(map_catalog_error)?;
    super::disposable_template_generation::refuse_unsettled(store).map_err(map_store_error)?;
    super::disposable_attempt_catalog::refuse_unsettled(store).map_err(map_store_error)?;
    super::lima_authority::refuse_unsettled_lima_authority(store).map_err(map_store_error)
}

fn require_non_message_stages_clean(
    store: &UnixPersonalWorkerStore,
) -> Result<(), ScaleSetInboxError> {
    store
        .refuse_unsettled_personal_worker_state()
        .map_err(map_catalog_error)?;
    super::disposable_template_generation::refuse_unsettled(store).map_err(map_store_error)?;
    super::lima_authority::refuse_unsettled_lima_authority(store).map_err(map_store_error)
}

fn map_store_error(error: PersonalWorkerStoreError) -> ScaleSetInboxError {
    let code = match error.kind() {
        PersonalWorkerStoreErrorKind::Busy => "inbox_busy",
        PersonalWorkerStoreErrorKind::VersionIncompatible => "inbox_version_incompatible",
        PersonalWorkerStoreErrorKind::RevisionConflict => "inbox_recovery_required",
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => "inbox_unsafe_filesystem",
        PersonalWorkerStoreErrorKind::Missing => "inbox_missing",
        PersonalWorkerStoreErrorKind::InvalidDocument
        | PersonalWorkerStoreErrorKind::CorruptState => "inbox_corrupt",
        PersonalWorkerStoreErrorKind::Io => "inbox_io",
    };
    ScaleSetInboxError::new(code)
}

fn map_catalog_error(
    error: crate::disposable_attempt_catalog::DisposableAttemptCatalogError,
) -> ScaleSetInboxError {
    use crate::disposable_attempt_catalog::DisposableAttemptCatalogErrorKind;

    let code = match error.kind() {
        DisposableAttemptCatalogErrorKind::Busy => "inbox_busy",
        DisposableAttemptCatalogErrorKind::VersionIncompatible => "inbox_version_incompatible",
        DisposableAttemptCatalogErrorKind::RecoveryRequired
        | DisposableAttemptCatalogErrorKind::Conflict => "inbox_recovery_required",
        DisposableAttemptCatalogErrorKind::UnsafeFilesystem => "inbox_unsafe_filesystem",
        DisposableAttemptCatalogErrorKind::Missing => "inbox_missing",
        DisposableAttemptCatalogErrorKind::CorruptState
        | DisposableAttemptCatalogErrorKind::IdentityDrift
        | DisposableAttemptCatalogErrorKind::InvalidAction
        | DisposableAttemptCatalogErrorKind::AlreadyExists
        | DisposableAttemptCatalogErrorKind::LimitExceeded => "inbox_corrupt",
        DisposableAttemptCatalogErrorKind::Io => "inbox_io",
    };
    ScaleSetInboxError::new(code)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogErrorKind, DisposableAttemptCatalogStore,
    };
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::DisposableWorkerResources;
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_bridge::{
        ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetStatistics,
    };
    use crate::github_scale_set_consumer::{
        ScaleSetConsumerPolicy, apply_scale_set_ack_outcome, apply_scale_set_event,
    };
    use crate::github_scale_set_protocol::ScaleSetJobId;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-scale-set-inbox-{label}-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create temporary state root");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))
                .expect("set private root mode");
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn event() -> ScaleSetBridgeEvent {
        ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
            runner_request_id: 41,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse("job-1").unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        })
    }

    fn source_identity() -> ScaleSetBridgeIdentity {
        ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "22".repeat(32))).unwrap()
    }

    fn initialized_store(root: &TempRoot) -> UnixPersonalWorkerStore {
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0)
            .expect("open durable store");
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().expect("initialize attempt catalog");
        catalog.into_store()
    }

    fn consumer_policy() -> ScaleSetConsumerPolicy {
        ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap()
    }

    fn statistics() -> ScaleSetStatistics {
        ScaleSetStatistics {
            available_jobs: 1,
            acquired_jobs: 0,
            assigned_jobs: 0,
            running_jobs: 0,
            registered_runners: 0,
            busy_runners: 0,
            idle_runners: 0,
        }
    }

    #[test]
    fn capacity_poll_and_message_publication_retain_one_canonical_lock() {
        let root = TempRoot::new("capacity-poll-lock");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        store.initialize_scale_set_inbox(&identity).unwrap();

        let (response, attempt_id) = store
            .poll_and_record_scale_set(&identity, |available_capacity| {
                assert_eq!(available_capacity, 1);
                let concurrent =
                    UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0);
                let error = match concurrent {
                    Ok(_) => {
                        panic!("cooperating catalog writer acquired the poll transaction lock")
                    }
                    Err(error) => error,
                };
                assert_eq!(error.kind(), DisposableAttemptCatalogErrorKind::Busy);
                Ok::<_, ()>((
                    ScaleSetBridgePoll::Message {
                        message_id: 7,
                        statistics: statistics(),
                        events: vec![event()],
                    },
                    EpochMillis::new(100_000).unwrap(),
                    EpochMillis::new(120_000).unwrap(),
                ))
            })
            .unwrap()
            .unwrap();

        assert!(attempt_id.is_none());
        assert!(matches!(
            response,
            ScaleSetBridgePoll::Message { message_id: 7, .. }
        ));
        let inbox = store.load_scale_set_inbox().unwrap().unwrap();
        assert_eq!(inbox.pending().unwrap().message_id(), 7);
    }

    #[test]
    fn inbox_initialization_and_successors_are_durable() {
        let root = TempRoot::new("successors");
        let mut store = initialized_store(&root);
        let empty = store
            .initialize_scale_set_inbox(&source_identity())
            .expect("initialize inbox");
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .expect("persist message before acknowledgement");
        assert_eq!(store.load_scale_set_inbox().unwrap(), Some(recorded));
        assert_eq!(
            DisposableAttemptCatalogStore::recover(&mut store)
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::Conflict
        );
    }

    #[test]
    fn clone_admission_requires_the_exact_settled_inbox_source() {
        let root = TempRoot::new("clone-admission-source");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();

        require_settled_source(&store, identity.as_str()).unwrap();
        let wrong = format!("sha256:{}", "99".repeat(32));
        assert_eq!(
            require_settled_source(&store, &wrong).unwrap_err().kind(),
            PersonalWorkerStoreErrorKind::InvalidDocument
        );

        let pending = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &pending)
            .unwrap();
        assert_eq!(
            require_settled_source(&store, identity.as_str())
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn exact_staged_successor_recovers_after_restart() {
        let root = TempRoot::new("recovery");
        let mut store = initialized_store(&root);
        let empty = store
            .initialize_scale_set_inbox(&source_identity())
            .expect("initialize inbox");
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        {
            let _lock = store.acquire_mutation_lock().unwrap();
            let mut staged = store.stage_scale_set_inbox(&recorded).unwrap();
            staged.disarm();
        }
        assert_eq!(
            store.load_scale_set_inbox().unwrap_err().code(),
            "inbox_recovery_required"
        );
        assert_eq!(
            DisposableAttemptCatalogStore::recover(&mut store)
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::Conflict
        );
        drop(store);
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0)
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::Conflict
        );
        let recovered = UnixPersonalWorkerStore::open_or_recover_scale_set_inbox(&root.0)
            .expect("reopen and recover inbox");
        assert_eq!(recovered.load_scale_set_inbox().unwrap(), Some(recorded));
    }

    #[test]
    fn inbox_initialization_requires_the_attempt_catalog() {
        let root = TempRoot::new("missing-catalog");
        let mut store = UnixPersonalWorkerStore::open_or_recover_scale_set_inbox(&root.0)
            .expect("open inbox store");
        assert_eq!(
            store
                .initialize_scale_set_inbox(&source_identity())
                .unwrap_err()
                .code(),
            "inbox_catalog_missing"
        );
    }

    #[test]
    fn missing_inbox_cannot_reset_nonempty_catalog_history() {
        let root = TempRoot::new("missing-inbox-with-history");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .unwrap();
        let policy = consumer_policy();
        store
            .apply_next_scale_set_event(
                &identity,
                recorded.revision(),
                |pending, event, catalog| {
                    crate::github_scale_set_consumer::apply_scale_set_event(
                        &policy,
                        pending,
                        event,
                        catalog,
                        EpochMillis::new(100_001).unwrap(),
                    )
                    .map(|next| (next, ()))
                },
            )
            .unwrap()
            .unwrap();
        std::fs::remove_file(
            root.0
                .join(super::super::STORE_DIRECTORY)
                .join(INBOX_DOCUMENT),
        )
        .unwrap();

        assert_eq!(
            store
                .initialize_scale_set_inbox(&identity)
                .unwrap_err()
                .code(),
            "inbox_catalog_history_without_inbox"
        );
        assert!(store.load_scale_set_inbox().unwrap().is_none());
    }

    #[test]
    fn catalog_effect_precedes_cursor_and_replay_is_idempotent() {
        let root = TempRoot::new("event-transaction");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .unwrap();
        let policy = consumer_policy();

        // Model a crash after the catalog successor became durable but before the inbox cursor.
        {
            let _lock = store.acquire_mutation_lock().unwrap();
            let catalog = store
                .load_catalog_named(super::super::disposable_attempt_catalog::CATALOG_DOCUMENT)
                .unwrap()
                .unwrap();
            let pending = recorded.pending().unwrap();
            let next = apply_scale_set_event(
                &policy,
                pending,
                pending.next_event().unwrap(),
                &catalog,
                EpochMillis::new(100_001).unwrap(),
            )
            .unwrap();
            let mut staged = store.stage_catalog(&next).unwrap();
            store
                .publish_named_staged(
                    &mut staged,
                    super::super::disposable_attempt_catalog::CATALOG_DOCUMENT,
                    false,
                )
                .unwrap();
        }

        let applied = store
            .apply_next_scale_set_event(
                &identity,
                recorded.revision(),
                |pending, event, catalog| {
                    assert_eq!(pending.not_after().get(), 120_000);
                    assert!(matches!(event, ScaleSetBridgeEvent::Available(_)));
                    apply_scale_set_event(
                        &policy,
                        pending,
                        event,
                        catalog,
                        EpochMillis::new(100_002).unwrap(),
                    )
                    .map(|next| (next, ()))
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(applied.0.pending().unwrap().next_event_index(), 1);
        assert_eq!(applied.1.active().len(), 1);
        assert_eq!(
            store
                .load_scale_set_inbox()
                .unwrap()
                .unwrap()
                .pending()
                .unwrap()
                .next_event_index(),
            1
        );
    }

    #[test]
    fn acknowledgement_is_checkpointed_before_bridge_call() {
        let root = TempRoot::new("ack-transaction");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .unwrap();
        let policy = consumer_policy();
        let (applied, applied_catalog, ()) = store
            .apply_next_scale_set_event(
                &identity,
                recorded.revision(),
                |pending, event, catalog| {
                    apply_scale_set_event(
                        &policy,
                        pending,
                        event,
                        catalog,
                        EpochMillis::new(100_001).unwrap(),
                    )
                    .map(|next| (next, ()))
                },
            )
            .unwrap()
            .unwrap();

        let path = root.0.join(STORE_DIRECTORY).join(INBOX_DOCUMENT);
        let completed = store
            .acknowledge_scale_set_message(
                &identity,
                applied.revision(),
                applied_catalog.revision(),
                |message_id, _, _| {
                    assert_eq!(message_id, 7);
                    let checkpoint =
                        decode_scale_set_inbox(&std::fs::read(&path).unwrap()).unwrap();
                    assert!(checkpoint.pending().unwrap().ack_started());
                    Ok::<_, ()>(vec![41])
                },
            )
            .unwrap()
            .unwrap();
        assert!(completed.pending().is_none());
        assert_eq!(completed.last_ack().unwrap().acquired_request_ids(), [41]);

        assert_eq!(
            DisposableAttemptCatalogStore::recover(&mut store)
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::Conflict
        );
        let (reconciled, acquired_catalog, ()) = store
            .apply_scale_set_ack_outcome(&identity, completed.revision(), |receipt, catalog| {
                apply_scale_set_ack_outcome(&policy, receipt, catalog).map(|next| (next, ()))
            })
            .unwrap()
            .unwrap();
        assert!(reconciled.last_ack().unwrap().outcome_applied());
        assert_eq!(
            acquired_catalog.active()[0].attempt().phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
        );
        let second = reconciled
            .record(
                8,
                EpochMillis::new(100_010).unwrap(),
                EpochMillis::new(120_010).unwrap(),
                Vec::new(),
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(reconciled.revision(), &second)
            .unwrap();
        let failed = store
            .acknowledge_scale_set_message(
                &identity,
                second.revision(),
                acquired_catalog.revision(),
                |_, _, _| Err::<Vec<u64>, _>("bridge-failed"),
            )
            .unwrap();
        assert_eq!(failed.unwrap_err(), "bridge-failed");
        assert!(
            store
                .load_scale_set_inbox()
                .unwrap()
                .unwrap()
                .pending()
                .unwrap()
                .ack_started()
        );
    }

    #[test]
    fn unacquired_offer_is_released_before_outcome_marker_and_replays_after_crash() {
        let root = TempRoot::new("ack-outcome-replay");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .unwrap();
        let policy = consumer_policy();
        let (applied, applied_catalog, ()) = store
            .apply_next_scale_set_event(
                &identity,
                recorded.revision(),
                |pending, event, catalog| {
                    apply_scale_set_event(
                        &policy,
                        pending,
                        event,
                        catalog,
                        EpochMillis::new(100_001).unwrap(),
                    )
                    .map(|next| (next, ()))
                },
            )
            .unwrap()
            .unwrap();
        let completed = store
            .acknowledge_scale_set_message(
                &identity,
                applied.revision(),
                applied_catalog.revision(),
                |_, _, _| Ok::<_, ()>(Vec::new()),
            )
            .unwrap()
            .unwrap();

        // Model a crash after the catalog release became durable but before the inbox marker.
        {
            let _lock = store.acquire_mutation_lock().unwrap();
            let catalog = store
                .load_catalog_named(super::super::disposable_attempt_catalog::CATALOG_DOCUMENT)
                .unwrap()
                .unwrap();
            let released =
                apply_scale_set_ack_outcome(&policy, completed.last_ack().unwrap(), &catalog)
                    .unwrap();
            let mut staged = store.stage_catalog(&released).unwrap();
            store
                .publish_named_staged(
                    &mut staged,
                    super::super::disposable_attempt_catalog::CATALOG_DOCUMENT,
                    false,
                )
                .unwrap();
        }

        let (reconciled, catalog, ()) = store
            .apply_scale_set_ack_outcome(&identity, completed.revision(), |receipt, catalog| {
                apply_scale_set_ack_outcome(&policy, receipt, catalog).map(|next| (next, ()))
            })
            .unwrap()
            .unwrap();
        assert!(reconciled.last_ack().unwrap().outcome_applied());
        assert_eq!(
            catalog.active()[0].attempt().phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::UnprovisionedReleasing
        );
        assert!(DisposableAttemptCatalogStore::recover(&mut store).is_ok());
    }

    #[test]
    fn expired_acquired_clone_authorization_waits_for_upstream_cancellation() {
        let root = TempRoot::new("expired-clone-authorization");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .unwrap();
        let policy = consumer_policy();
        let (applied, reserved, ()) = store
            .apply_next_scale_set_event(
                &identity,
                recorded.revision(),
                |pending, event, catalog| {
                    apply_scale_set_event(
                        &policy,
                        pending,
                        event,
                        catalog,
                        EpochMillis::new(100_001).unwrap(),
                    )
                    .map(|next| (next, ()))
                },
            )
            .unwrap()
            .unwrap();
        let acknowledged = store
            .acknowledge_scale_set_message(
                &identity,
                applied.revision(),
                reserved.revision(),
                |_, _, _| Ok::<_, ()>(vec![41]),
            )
            .unwrap()
            .unwrap();
        let (settled, reserved, ()) = store
            .apply_scale_set_ack_outcome(&identity, acknowledged.revision(), |receipt, catalog| {
                apply_scale_set_ack_outcome(&policy, receipt, catalog).map(|next| (next, ()))
            })
            .unwrap()
            .unwrap();
        assert!(settled.last_ack().unwrap().outcome_applied());
        let attempt_id = reserved.active()[0].attempt().attempt_id().clone();
        let authorized = reserved
            .replace_attempt(
                &attempt_id,
                reserved.active()[0].attempt().revision(),
                crate::disposable_attempt_catalog::DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        DisposableAttemptCatalogStore::replace_if_revision(
            &mut store,
            reserved.revision(),
            &authorized,
        )
        .unwrap();

        assert!(
            !store
                .checkpoint_expired_scale_set_preclone_attempt(
                    &identity,
                    &attempt_id,
                    EpochMillis::new(21_700_001).unwrap(),
                )
                .unwrap()
        );
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog.active()[0].attempt().phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::CloneAuthorized
        );
    }

    #[test]
    fn expired_acquired_job_cannot_take_the_unprovisioned_release_path() {
        let root = TempRoot::new("expired-acquired-job");
        let mut store = initialized_store(&root);
        let identity = source_identity();
        let empty = store.initialize_scale_set_inbox(&identity).unwrap();
        let recorded = empty
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event()],
            )
            .unwrap();
        store
            .replace_scale_set_inbox_if_revision(empty.revision(), &recorded)
            .unwrap();
        let policy = consumer_policy();
        let (applied, reserved, ()) = store
            .apply_next_scale_set_event(
                &identity,
                recorded.revision(),
                |pending, event, catalog| {
                    apply_scale_set_event(
                        &policy,
                        pending,
                        event,
                        catalog,
                        EpochMillis::new(100_001).unwrap(),
                    )
                    .map(|next| (next, ()))
                },
            )
            .unwrap()
            .unwrap();
        let acknowledged = store
            .acknowledge_scale_set_message(
                &identity,
                applied.revision(),
                reserved.revision(),
                |_, _, _| Ok::<_, ()>(vec![41]),
            )
            .unwrap()
            .unwrap();
        let (settled, acquired, ()) = store
            .apply_scale_set_ack_outcome(&identity, acknowledged.revision(), |receipt, catalog| {
                apply_scale_set_ack_outcome(&policy, receipt, catalog).map(|next| (next, ()))
            })
            .unwrap()
            .unwrap();
        assert!(settled.last_ack().unwrap().outcome_applied());
        let attempt_id = acquired.active()[0].attempt().attempt_id().clone();

        assert!(
            !store
                .checkpoint_expired_scale_set_preclone_attempt(
                    &identity,
                    &attempt_id,
                    EpochMillis::new(21_700_001).unwrap(),
                )
                .unwrap()
        );
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog.active()[0].attempt().phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
        );
        assert_eq!(
            catalog.active()[0]
                .attempt()
                .github_job_id()
                .unwrap()
                .as_str(),
            "job-1"
        );
    }
}
