use std::fmt;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, AtFlags, Mode, RenameFlags};
use rustix::io::Errno;
use sha2::{Digest as _, Sha256};

use super::{
    CURRENT_DOCUMENT, DIRECTORY_FLAGS, EXISTING_FILE_FLAGS, NEW_FILE_FLAGS, PRIVATE_FILE_MODE,
    STAGED_DOCUMENT, STORE_DIRECTORY, StagedDocument, StoreMutationLock, UnixPersonalWorkerStore,
    acquire_mutation_lock_in, inspect_directory, inspect_private_file,
    map_existing_store_directory_open_error, map_root_open_error, store_error,
    synchronize_directory,
};
use crate::artifact::Sha256Digest;
use crate::execution_admission::EpochMillis;
use crate::lima_lifecycle::{LimaLifecycleState, LimaResourceProfile};
use crate::personal_worker_lima_authority::{
    ConfirmedPersonalWorkerLimaEnrollment, MAX_PERSONAL_WORKER_LIMA_AUTHORITY_BYTES,
    PersonalWorkerLimaAttemptPhase, PersonalWorkerLimaAuthorityDocument,
    PersonalWorkerLimaAuthorityErrorKind, PersonalWorkerLimaAuthorityGeneration,
    decode_personal_worker_lima_authority, encode_personal_worker_lima_authority,
};
use crate::personal_worker_queue::{
    PersonalWorkerProfile, PersonalWorkerProfileObservation, PersonalWorkerQueueGeneration,
};
use crate::personal_worker_store::{
    PersonalWorkerStoreDocument, PersonalWorkerStoreError, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreRevision, encode_personal_worker_store_document,
};

const AUTHORITY_DOCUMENT: &str = "lima-authority.json";
const STAGED_AUTHORITY_DOCUMENT: &str = ".lima-authority.next.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixPersonalWorkerLimaAuthorityErrorKind {
    InvalidDocument,
    RevisionConflict,
    RecoveryRequired,
    Busy,
    Missing,
    Io,
    UnsafeFilesystem,
    VersionIncompatible,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixPersonalWorkerLimaAuthorityError {
    kind: UnixPersonalWorkerLimaAuthorityErrorKind,
    message: &'static str,
}

impl UnixPersonalWorkerLimaAuthorityError {
    #[must_use]
    pub const fn kind(&self) -> UnixPersonalWorkerLimaAuthorityErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    fn into_store_error(self) -> PersonalWorkerStoreError {
        let kind = match self.kind {
            SelfKind::InvalidDocument => PersonalWorkerStoreErrorKind::InvalidDocument,
            SelfKind::RevisionConflict | SelfKind::RecoveryRequired => {
                PersonalWorkerStoreErrorKind::RevisionConflict
            }
            SelfKind::Busy => PersonalWorkerStoreErrorKind::Busy,
            SelfKind::Missing => PersonalWorkerStoreErrorKind::Missing,
            SelfKind::Io => PersonalWorkerStoreErrorKind::Io,
            SelfKind::UnsafeFilesystem => PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            SelfKind::VersionIncompatible => PersonalWorkerStoreErrorKind::VersionIncompatible,
            SelfKind::CorruptState => PersonalWorkerStoreErrorKind::CorruptState,
        };
        store_error(kind, self.message)
    }
}

impl fmt::Display for UnixPersonalWorkerLimaAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for UnixPersonalWorkerLimaAuthorityError {}

impl From<PersonalWorkerStoreError> for UnixPersonalWorkerLimaAuthorityError {
    fn from(error: PersonalWorkerStoreError) -> Self {
        let kind = match error.kind() {
            PersonalWorkerStoreErrorKind::InvalidDocument => SelfKind::InvalidDocument,
            PersonalWorkerStoreErrorKind::RevisionConflict => SelfKind::RevisionConflict,
            PersonalWorkerStoreErrorKind::Busy => SelfKind::Busy,
            PersonalWorkerStoreErrorKind::Missing => SelfKind::Missing,
            PersonalWorkerStoreErrorKind::Io => SelfKind::Io,
            PersonalWorkerStoreErrorKind::UnsafeFilesystem => SelfKind::UnsafeFilesystem,
            PersonalWorkerStoreErrorKind::VersionIncompatible => SelfKind::VersionIncompatible,
            PersonalWorkerStoreErrorKind::CorruptState => SelfKind::CorruptState,
        };
        Self {
            kind,
            message: error.message(),
        }
    }
}

type SelfKind = UnixPersonalWorkerLimaAuthorityErrorKind;
type AuthorityResult<T> = Result<T, UnixPersonalWorkerLimaAuthorityError>;

#[derive(Debug)]
pub struct UnixPersonalWorkerLimaAuthorityGuard {
    store: UnixPersonalWorkerStore,
    _lock: StoreMutationLock,
    worker: PersonalWorkerStoreDocument,
    authority: Option<PersonalWorkerLimaAuthorityDocument>,
    uncertain: bool,
}

impl UnixPersonalWorkerLimaAuthorityGuard {
    /// Open the existing worker store, acquire its canonical exclusive lock without blocking, and
    /// recover both worker and Lima-authority stages before returning any authorization evidence.
    pub fn open(root_path: impl AsRef<Path>) -> AuthorityResult<Self> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = fs::openat(&root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_existing_store_directory_open_error)?;
        inspect_directory(&directory, "personal worker store directory", Some(owner))?;
        let mut store = UnixPersonalWorkerStore {
            _root: root,
            directory,
            owner,
        };
        let lock = acquire_mutation_lock_in(&store.directory, owner)?;
        synchronize_directory(&store._root, "personal worker state root")?;
        // A previous writer can have returned an ambiguous error after publishing either a worker
        // or authority entry. Close the child-directory durability window before trusting any
        // visible entry, then classify both documents before recovering either one.
        synchronize_directory(&store.directory, "personal worker store directory")?;
        let current_authority = load_authority(&store, AUTHORITY_DOCUMENT)?;
        let staged_authority = load_authority(&store, STAGED_AUTHORITY_DOCUMENT)?;
        let current_worker = store.load_named(CURRENT_DOCUMENT)?;
        let worker_plan = store.recovery_plan()?;
        let authority_present = current_authority.is_some() || staged_authority.is_some();
        let authority_unsettled = staged_authority.is_some()
            || current_authority
                .as_ref()
                .is_some_and(|document| document.attempt().is_some());
        if authority_present && current_worker.is_none() {
            return Err(authority_recovery_required());
        }
        let settlement_prepared = current_authority
            .as_ref()
            .is_some_and(|document| document.settlement().is_some());
        if authority_unsettled
            && !settlement_prepared
            && !matches!(worker_plan, super::StoreRecoveryPlan::Clean { .. })
        {
            return Err(authority_recovery_required());
        }
        if !authority_unsettled && !settlement_prepared {
            store.recover_locked()?;
        }
        let mut worker = store.load_named(CURRENT_DOCUMENT)?.ok_or_else(|| {
            store_error(
                PersonalWorkerStoreErrorKind::Missing,
                "personal worker state does not exist",
            )
        })?;
        if settlement_prepared {
            recover_prepared_settlement(
                &mut store,
                &mut worker,
                current_authority
                    .as_ref()
                    .expect("settlement authority exists"),
            )?;
        } else {
            recover_authority(&store, &worker)?;
        }
        let mut authority = load_authority(&store, AUTHORITY_DOCUMENT)?;
        if authority
            .as_ref()
            .is_some_and(|document| document.settlement().is_some())
        {
            recover_prepared_settlement(
                &mut store,
                &mut worker,
                authority.as_ref().expect("settlement authority exists"),
            )?;
            authority = load_authority(&store, AUTHORITY_DOCUMENT)?;
        }
        if authority
            .as_ref()
            .is_some_and(|document| !attempt_matches_worker(document, &worker))
        {
            return Err(authority_recovery_required());
        }
        Ok(Self {
            store,
            _lock: lock,
            worker,
            authority,
            uncertain: false,
        })
    }

    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.worker.revision()
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.worker.queue().generation
    }

    #[must_use]
    pub const fn authority(&self) -> Option<&PersonalWorkerLimaAuthorityDocument> {
        self.authority.as_ref()
    }

    #[must_use]
    pub fn has_active_work(&self) -> bool {
        !self.worker.queue().active.is_empty()
    }

    #[must_use]
    pub const fn recovery_required(&self) -> bool {
        self.uncertain
    }

    /// Publish the exact confirmed generation-one enrollment without replacing prior authority.
    pub fn publish_enrollment(
        &mut self,
        enrollment: ConfirmedPersonalWorkerLimaEnrollment,
        current_time: EpochMillis,
    ) -> AuthorityResult<()> {
        if self.uncertain {
            return Err(authority_recovery_required());
        }
        if !enrollment.is_fresh_at(current_time) {
            return Err(authority_stale_enrollment());
        }
        let document = enrollment.into_document();
        if self.authority.is_some()
            || document.authority_generation().get() != 1
            || document.attempt().is_some()
        {
            return Err(authority_conflict());
        }
        if let Err(error) = publish_authority(&self.store, &document, true) {
            self.uncertain = true;
            return Err(error);
        }
        self.authority = Some(document);
        Ok(())
    }

    /// Replace one authority generation while retaining this exact worker-store writer lock.
    pub fn replace_authority(
        &mut self,
        expected_generation: PersonalWorkerLimaAuthorityGeneration,
        document: &PersonalWorkerLimaAuthorityDocument,
    ) -> AuthorityResult<()> {
        if self.uncertain {
            return Err(authority_recovery_required());
        }
        let current = self.authority.as_ref().ok_or_else(authority_missing)?;
        if current.authority_generation() != expected_generation {
            return Err(authority_conflict());
        }
        if current.settlement().is_some() || document.settlement().is_some() {
            return Err(authority_conflict());
        }
        document
            .validate_successor_of(current)
            .map_err(|_| authority_conflict())?;
        if current.attempt().is_none() {
            let attempt = document.attempt().ok_or_else(authority_conflict)?;
            if self.has_active_work()
                || attempt.store_revision() != self.worker.revision()
                || attempt.queue_generation() != self.worker.queue().generation
            {
                return Err(authority_conflict());
            }
        }
        if let Err(error) = publish_authority(&self.store, document, false) {
            self.uncertain = true;
            return Err(error);
        }
        self.authority = Some(document.clone());
        Ok(())
    }

    /// Publish the exact completed profile observation and clear its lifecycle attempt.
    pub fn settle_completed_attempt(&mut self) -> AuthorityResult<()> {
        if self.uncertain {
            return Err(authority_recovery_required());
        }
        let current = self.authority.as_ref().ok_or_else(authority_missing)?;
        if current.settlement().is_some() {
            return Err(authority_recovery_required());
        }
        let successor = settlement_worker_successor(&self.worker, current)?;
        let prepared = current
            .prepare_settlement(
                worker_document_digest(&self.worker)?,
                worker_document_digest(&successor)?,
            )
            .map_err(|_| authority_conflict())?;
        if let Err(error) = publish_authority(&self.store, &prepared, false) {
            self.uncertain = true;
            return Err(error);
        }
        self.authority = Some(prepared.clone());
        if let Err(error) =
            recover_prepared_settlement(&mut self.store, &mut self.worker, &prepared)
        {
            self.uncertain = true;
            return Err(error);
        }
        self.authority = load_authority(&self.store, AUTHORITY_DOCUMENT)?;
        Ok(())
    }
}

fn recover_prepared_settlement(
    store: &mut UnixPersonalWorkerStore,
    worker: &mut PersonalWorkerStoreDocument,
    authority: &PersonalWorkerLimaAuthorityDocument,
) -> AuthorityResult<()> {
    let settlement = authority
        .settlement()
        .ok_or_else(authority_recovery_required)?;
    let cleared = authority
        .clear_settled_attempt()
        .map_err(|_| authority_recovery_required())?;
    let mut staged_authority = load_open_authority(store, STAGED_AUTHORITY_DOCUMENT)?;
    if staged_authority
        .as_ref()
        .is_some_and(|staged| staged.document != *authority && staged.document != cleared)
    {
        return Err(authority_recovery_required());
    }
    if staged_authority
        .as_ref()
        .is_some_and(|staged| staged.document == *authority)
    {
        remove_authority_stage(store)?;
        staged_authority = None;
    }

    let current_digest = worker_document_digest(worker)?;
    if current_digest == *settlement.previous_worker_digest() {
        if staged_authority.is_some() {
            return Err(authority_recovery_required());
        }
        let successor = settlement_worker_successor(worker, authority)?;
        if worker_document_digest(&successor)? != *settlement.successor_worker_digest()
            || successor.revision() != settlement.successor_store_revision()
            || successor.queue().generation != settlement.successor_queue_generation()
        {
            return Err(authority_recovery_required());
        }
        if let Some(staged_worker) = store.load_named(STAGED_DOCUMENT)? {
            if staged_worker != successor {
                return Err(authority_recovery_required());
            }
            store.synchronize_existing_staged(&staged_worker)?;
            let mut stage = StagedDocument::existing(store.directory.as_fd(), STAGED_DOCUMENT);
            store.publish_staged(&mut stage, false)?;
        } else {
            let mut stage = store.stage_document(&successor)?;
            store.publish_staged(&mut stage, false)?;
        }
        *worker = successor;
    } else if current_digest == *settlement.successor_worker_digest()
        && worker.revision() == settlement.successor_store_revision()
        && worker.queue().generation == settlement.successor_queue_generation()
    {
        if let Some(staged_worker) = store.load_named(STAGED_DOCUMENT)? {
            if staged_worker != *worker {
                return Err(authority_recovery_required());
            }
            store.remove_staged()?;
        }
    } else {
        return Err(authority_recovery_required());
    }

    match staged_authority {
        Some(staged) if staged.document == cleared => publish_existing_stage(store, &staged, false),
        Some(_) => Err(authority_recovery_required()),
        None => publish_authority(store, &cleared, false),
    }
}

fn settlement_worker_successor(
    worker: &PersonalWorkerStoreDocument,
    authority: &PersonalWorkerLimaAuthorityDocument,
) -> AuthorityResult<PersonalWorkerStoreDocument> {
    let attempt = authority
        .attempt()
        .filter(|attempt| attempt.phase() == PersonalWorkerLimaAttemptPhase::Completed)
        .ok_or_else(authority_recovery_required)?;
    if !worker.queue().active.is_empty()
        || worker.revision() != attempt.store_revision()
        || worker.queue().generation != attempt.queue_generation()
    {
        return Err(authority_recovery_required());
    }
    let profile = match attempt.after_state() {
        LimaLifecycleState::Stopped => PersonalWorkerProfile::Stopped,
        LimaLifecycleState::Running => match attempt.after_profile() {
            LimaResourceProfile::Interactive => PersonalWorkerProfile::Interactive,
            LimaResourceProfile::Work => PersonalWorkerProfile::Work,
        },
        LimaLifecycleState::Starting
        | LimaLifecycleState::Draining
        | LimaLifecycleState::Stopping
        | LimaLifecycleState::Unavailable => {
            return Err(authority_recovery_required());
        }
    };
    if worker
        .queue()
        .pending_profile_change
        .is_some_and(|pending| pending.target != profile)
    {
        return Err(authority_recovery_required());
    }
    let completed_at = attempt
        .completed_at()
        .ok_or_else(authority_recovery_required)?;
    let mut queue = worker.queue().clone();
    queue.generation = queue
        .generation
        .next()
        .map_err(|_| authority_recovery_required())?;
    queue.observed_at = completed_at;
    queue.profile_observation = PersonalWorkerProfileObservation::observed(profile);
    queue.pending_profile_change = None;
    worker
        .advance(queue, worker.cache_leases().to_vec())
        .map_err(Into::into)
}

fn worker_document_digest(document: &PersonalWorkerStoreDocument) -> AuthorityResult<Sha256Digest> {
    let encoded = encode_personal_worker_store_document(document)?;
    let digest = Sha256::digest(encoded);
    Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| authority_corrupt())
}

fn recover_authority(
    store: &UnixPersonalWorkerStore,
    worker: &PersonalWorkerStoreDocument,
) -> AuthorityResult<()> {
    let staged = load_open_authority(store, STAGED_AUTHORITY_DOCUMENT)?;
    let current = load_authority(store, AUTHORITY_DOCUMENT)?;
    match (current, staged) {
        (_, None) => Ok(()),
        (None, Some(staged))
            if staged.document.authority_generation().get() == 1
                && staged.document.attempt().is_none() =>
        {
            publish_existing_stage(store, &staged, true)
        }
        (Some(current), Some(staged)) => {
            if !attempt_matches_worker(&current, worker)
                || !attempt_matches_worker(&staged.document, worker)
            {
                Err(authority_recovery_required())
            } else if staged.document == current {
                remove_authority_stage(store)
            } else if staged.document.validate_successor_of(&current).is_ok() {
                publish_existing_stage(store, &staged, false)
            } else {
                Err(authority_recovery_required())
            }
        }
        _ => Err(authority_recovery_required()),
    }
}

fn attempt_matches_worker(
    authority: &PersonalWorkerLimaAuthorityDocument,
    worker: &PersonalWorkerStoreDocument,
) -> bool {
    authority.attempt().is_none_or(|attempt| {
        worker.queue().active.is_empty()
            && attempt.store_revision() == worker.revision()
            && attempt.queue_generation() == worker.queue().generation
    })
}

pub(super) fn refuse_unsettled_lima_authority(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    synchronize_directory(&store.directory, "personal worker store directory")?;
    let staged = load_authority(store, STAGED_AUTHORITY_DOCUMENT)
        .map_err(UnixPersonalWorkerLimaAuthorityError::into_store_error)?;
    let current = load_authority(store, AUTHORITY_DOCUMENT)
        .map_err(UnixPersonalWorkerLimaAuthorityError::into_store_error)?;
    // Orphan detection needs only safe, bounded file presence. Decoding here would preempt the
    // explicit schema-v1 migration path, which exclusively owns version conversion.
    let worker_present = store.read_named_bytes(CURRENT_DOCUMENT)?.is_some();
    if staged.is_some()
        || current
            .as_ref()
            .is_some_and(|document| document.attempt().is_some())
        || (current.is_some() && !worker_present)
    {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "a Lima lifecycle attempt requires recovery before worker mutation",
        ));
    }
    Ok(())
}

fn load_authority(
    store: &UnixPersonalWorkerStore,
    name: &str,
) -> AuthorityResult<Option<PersonalWorkerLimaAuthorityDocument>> {
    Ok(load_open_authority(store, name)?.map(|loaded| loaded.document))
}

struct OpenAuthorityDocument {
    document: PersonalWorkerLimaAuthorityDocument,
    file: OwnedFd,
    encoded_len: usize,
}

fn load_open_authority(
    store: &UnixPersonalWorkerStore,
    name: &str,
) -> AuthorityResult<Option<OpenAuthorityDocument>> {
    inspect_directory(
        &store.directory,
        "personal worker store directory",
        Some(store.owner),
    )?;
    let file = match fs::openat(&store.directory, name, EXISTING_FILE_FLAGS, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(authority_io()),
    };
    inspect_private_file(&file, store.owner, "Lima authority document", None)?;
    let mut file = File::from(file);
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take((MAX_PERSONAL_WORKER_LIMA_AUTHORITY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| authority_io())?;
    if bytes.len() > MAX_PERSONAL_WORKER_LIMA_AUTHORITY_BYTES {
        return Err(authority_corrupt());
    }
    let document = decode_personal_worker_lima_authority(&bytes).map_err(|error| {
        if error.kind == PersonalWorkerLimaAuthorityErrorKind::VersionIncompatible {
            store_error(
                PersonalWorkerStoreErrorKind::VersionIncompatible,
                "Lima authority requires an explicit supported migration",
            )
            .into()
        } else {
            authority_corrupt()
        }
    })?;
    Ok(Some(OpenAuthorityDocument {
        document,
        file: file.into(),
        encoded_len: bytes.len(),
    }))
}

fn publish_authority(
    store: &UnixPersonalWorkerStore,
    document: &PersonalWorkerLimaAuthorityDocument,
    no_replace: bool,
) -> AuthorityResult<()> {
    let encoded =
        encode_personal_worker_lima_authority(document).map_err(|_| authority_corrupt())?;
    let file = fs::openat(
        &store.directory,
        STAGED_AUTHORITY_DOCUMENT,
        NEW_FILE_FLAGS,
        PRIVATE_FILE_MODE,
    )
    .map_err(|error| {
        if error == Errno::EXIST {
            authority_recovery_required()
        } else {
            authority_io()
        }
    })?;
    let mut stage = AuthorityStage {
        directory: store.directory.as_fd(),
        armed: true,
    };
    fs::fchmod(&file, PRIVATE_FILE_MODE).map_err(|_| authority_io())?;
    inspect_private_file(
        &file,
        store.owner,
        "staged Lima authority document",
        Some(0),
    )?;
    let mut file = File::from(file);
    file.write_all(&encoded).map_err(|_| authority_io())?;
    file.sync_all().map_err(|_| authority_io())?;
    inspect_private_file(
        file.as_fd(),
        store.owner,
        "staged Lima authority document",
        Some(encoded.len()),
    )?;
    let flags = if no_replace {
        RenameFlags::NOREPLACE
    } else {
        RenameFlags::empty()
    };
    fs::renameat_with(
        &store.directory,
        STAGED_AUTHORITY_DOCUMENT,
        &store.directory,
        AUTHORITY_DOCUMENT,
        flags,
    )
    .map_err(|error| {
        if no_replace && error == Errno::EXIST {
            authority_conflict()
        } else {
            authority_io()
        }
    })?;
    stage.armed = false;
    synchronize_directory(&store.directory, "personal worker store directory")?;
    Ok(())
}

fn publish_existing_stage(
    store: &UnixPersonalWorkerStore,
    staged: &OpenAuthorityDocument,
    no_replace: bool,
) -> AuthorityResult<()> {
    fs::fsync(&staged.file).map_err(|_| authority_io())?;
    inspect_private_file(
        &staged.file,
        store.owner,
        "staged Lima authority document",
        Some(staged.encoded_len),
    )?;
    let flags = if no_replace {
        RenameFlags::NOREPLACE
    } else {
        RenameFlags::empty()
    };
    fs::renameat_with(
        &store.directory,
        STAGED_AUTHORITY_DOCUMENT,
        &store.directory,
        AUTHORITY_DOCUMENT,
        flags,
    )
    .map_err(|error| {
        if no_replace && error == Errno::EXIST {
            authority_recovery_required()
        } else {
            authority_io()
        }
    })?;
    synchronize_directory(&store.directory, "personal worker store directory")?;
    Ok(())
}

fn remove_authority_stage(store: &UnixPersonalWorkerStore) -> AuthorityResult<()> {
    fs::unlinkat(
        &store.directory,
        STAGED_AUTHORITY_DOCUMENT,
        AtFlags::empty(),
    )
    .map_err(|_| authority_io())?;
    synchronize_directory(&store.directory, "personal worker store directory")?;
    Ok(())
}

struct AuthorityStage<'a> {
    directory: BorrowedFd<'a>,
    armed: bool,
}

impl Drop for AuthorityStage<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.directory, STAGED_AUTHORITY_DOCUMENT, AtFlags::empty());
        }
    }
}

fn authority_error(
    kind: UnixPersonalWorkerLimaAuthorityErrorKind,
    message: &'static str,
) -> UnixPersonalWorkerLimaAuthorityError {
    UnixPersonalWorkerLimaAuthorityError { kind, message }
}

fn authority_missing() -> UnixPersonalWorkerLimaAuthorityError {
    authority_error(SelfKind::Missing, "Lima authority does not exist")
}

fn authority_conflict() -> UnixPersonalWorkerLimaAuthorityError {
    authority_error(
        SelfKind::RevisionConflict,
        "Lima authority changed or conflicts",
    )
}

fn authority_recovery_required() -> UnixPersonalWorkerLimaAuthorityError {
    authority_error(
        SelfKind::RecoveryRequired,
        "Lima authority requires recovery",
    )
}

fn authority_stale_enrollment() -> UnixPersonalWorkerLimaAuthorityError {
    authority_error(
        SelfKind::InvalidDocument,
        "confirmed Lima enrollment evidence is not fresh at publication",
    )
}

fn authority_corrupt() -> UnixPersonalWorkerLimaAuthorityError {
    authority_error(SelfKind::CorruptState, "Lima authority is corrupt")
}

fn authority_io() -> UnixPersonalWorkerLimaAuthorityError {
    authority_error(SelfKind::Io, "Lima authority persistence failed")
}
