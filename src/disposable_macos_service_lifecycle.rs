//! Crash-safe root lifecycle journal for the production macOS disposable-worker services.
//!
//! This store owns only the versioned public lifecycle journal. It deliberately does not execute
//! account, filesystem, PF, Keychain, or launchd mutations. A later action-specific transaction
//! must publish `executing`, perform and observe exactly one external action while retaining this
//! lock, then publish `completed`. An ambiguous outcome therefore remains explicit recovery debt
//! and is never converted into automatic rollback authority.

#![allow(
    dead_code,
    reason = "this store is consumed by the next production root-apply transaction slice"
)]

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, Stat};
use serde::Serialize;

use crate::disposable_macos_service_installation::{
    DisposableMacosServiceActionKind, DisposableMacosServicePlan,
};
use crate::journal_document::{
    JournalStateDocument, decode_journal_document, encode_journal_document,
};

const LIFECYCLE_DOCUMENT: &str = "disposable-service-lifecycle-v1.json";
const PREPARED_LIFECYCLE_DOCUMENT: &str = ".disposable-service-lifecycle.next.json";
const STAGE_PREFIX: &str = ".disposable-service-lifecycle.stage-";
const LOCK_DOCUMENT: &str = "disposable-service-lifecycle.lock";
const RANDOM_SOURCE: &str = "/dev/urandom";
const MAX_LIFECYCLE_BYTES: u64 = 128 * 1024;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DisposableMacosServiceLifecycleErrorKind {
    Busy,
    Missing,
    UnsafeState,
    RecoveryRequired,
    Io,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct DisposableMacosServiceLifecycleError {
    kind: DisposableMacosServiceLifecycleErrorKind,
    code: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableMacosServiceActionRecovery {
    Completed,
    RetryAuthorized,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableMacosServiceActionConfirmation {
    Completed,
    Unknown,
}

/// Action-specific authority below the lifecycle journal.
///
/// Implementations must retain exact evidence in `Prepared`, reconfirm it immediately before a
/// mutation in `execute`, and classify recovery as `RetryAuthorized` only after proving that no
/// earlier external operation can still take effect. The journal never manufactures that proof.
pub(crate) trait DisposableMacosServiceActionDriver {
    type Prepared;
    type Error;

    fn recover(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
    ) -> Result<DisposableMacosServiceActionRecovery, Self::Error>;

    fn prepare(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
    ) -> Result<Self::Prepared, Self::Error>;

    fn execute(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
        prepared: Self::Prepared,
    ) -> Result<(), Self::Error>;

    fn confirm_completed(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
    ) -> Result<DisposableMacosServiceActionConfirmation, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisposableMacosServiceLifecycleDisposition {
    ActionCompleted {
        action: DisposableMacosServiceActionKind,
        document: JournalStateDocument,
    },
    RecoveryRequired {
        action: DisposableMacosServiceActionKind,
        document: JournalStateDocument,
    },
    Settled {
        document: JournalStateDocument,
    },
}

impl DisposableMacosServiceLifecycleError {
    pub(crate) const fn kind(self) -> DisposableMacosServiceLifecycleErrorKind {
        self.kind
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableMacosServiceLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableMacosServiceLifecycleError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableMacosServiceLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the production service lifecycle journal is unavailable")
    }
}

impl std::error::Error for DisposableMacosServiceLifecycleError {}

#[derive(Debug)]
struct LifecycleLock {
    file: OwnedFd,
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        // CLOEXEC does not prevent a concurrent fork from briefly retaining this open-file
        // description. Explicit unlock prevents that duplicate from extending the transaction.
        let _ = fs::flock(&self.file, FlockOperation::Unlock);
    }
}

struct ExactDocument {
    document: JournalStateDocument,
    file: File,
    snapshot: FileSnapshot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    size: i64,
    modified_seconds: i64,
    modified_nanoseconds: u64,
    changed_seconds: i64,
    changed_nanoseconds: u64,
}

impl FileSnapshot {
    // rustix exposes the libc stat fields with different integer types on macOS and Linux.
    #[allow(clippy::useless_conversion)]
    fn from_stat(stat: &Stat) -> Self {
        Self {
            device: u64::try_from(stat.st_dev).unwrap_or(u64::MAX),
            inode: stat.st_ino,
            owner: stat.st_uid,
            group: stat.st_gid,
            mode: u32::from(stat.st_mode),
            links: u64::from(stat.st_nlink),
            size: stat.st_size,
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: u64::try_from(stat.st_mtime_nsec).unwrap_or(u64::MAX),
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: u64::try_from(stat.st_ctime_nsec).unwrap_or(u64::MAX),
        }
    }
}

/// Descriptor-bound store for one active production service lifecycle operation.
pub(crate) struct DisposableMacosServiceLifecycleStore {
    directory: OwnedFd,
    path: PathBuf,
    owner: (u32, u32),
    identity: DirectoryIdentity,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
}

impl DirectoryIdentity {
    #[allow(clippy::useless_conversion)]
    fn from_stat(stat: &Stat) -> Self {
        Self {
            device: u64::try_from(stat.st_dev).unwrap_or(u64::MAX),
            inode: stat.st_ino,
            owner: stat.st_uid,
            group: stat.st_gid,
            mode: u32::from(stat.st_mode),
        }
    }
}

impl fmt::Debug for DisposableMacosServiceLifecycleStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableMacosServiceLifecycleStore")
            .finish_non_exhaustive()
    }
}

impl DisposableMacosServiceLifecycleStore {
    /// Open an already-created private lifecycle directory.
    ///
    /// Production will call this with root/wheel after the separately reviewed root-directory
    /// creation boundary. Keeping creation out of this slice makes its ownership assumptions
    /// explicit and lets all crash semantics run in unprivileged fixtures.
    pub(crate) fn open_existing(
        path: &Path,
        owner: (u32, u32),
    ) -> Result<Self, DisposableMacosServiceLifecycleError> {
        let directory =
            fs::open(path, DIRECTORY_FLAGS, Mode::empty()).map_err(|error| match error {
                rustix::io::Errno::NOENT => lifecycle_error(
                    DisposableMacosServiceLifecycleErrorKind::Missing,
                    "disposable_service_lifecycle_directory_missing",
                ),
                rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => unsafe_state(),
                _ => io_error(),
            })?;
        let held = fs::fstat(&directory).map_err(|_| io_error())?;
        inspect_directory(&held, owner)?;
        let resolved = fs::stat(path).map_err(|_| unsafe_state())?;
        if !same_directory(&held, &resolved) {
            return Err(unsafe_state());
        }
        Ok(Self {
            directory,
            path: path.to_path_buf(),
            owner,
            identity: DirectoryIdentity::from_stat(&held),
        })
    }

    /// Recover a prepared publication and create the exact initial journal when absent.
    pub(crate) fn initialize_or_recover(
        &self,
        plan: &DisposableMacosServicePlan,
    ) -> Result<JournalStateDocument, DisposableMacosServiceLifecycleError> {
        let _lock = self.acquire_lock()?;
        self.synchronize_directory()?;
        if let Some(current) = self.recover_locked(plan)? {
            return Ok(current.document);
        }
        let initial = plan.initial_journal_document();
        self.publish_locked(plan, None, &initial)?;
        self.read_current(plan)?
            .map(|current| current.document)
            .ok_or_else(recovery_required)
    }

    /// Reconcile at most one exact external action while retaining the lifecycle lock.
    ///
    /// A new action is prepared before its `executing` checkpoint and executed only after that
    /// checkpoint is durable. A restarted `executing` action is never replayed unless the
    /// action-specific driver proves retry safety. Errors and unknown postconditions leave the
    /// exact action executing for later recovery; this method performs no automatic rollback.
    pub(crate) fn reconcile_one<D: DisposableMacosServiceActionDriver>(
        &self,
        plan: &DisposableMacosServicePlan,
        driver: &mut D,
    ) -> Result<
        Result<DisposableMacosServiceLifecycleDisposition, D::Error>,
        DisposableMacosServiceLifecycleError,
    > {
        let _lock = self.acquire_lock()?;
        self.synchronize_directory()?;
        let mut current = match self.recover_locked(plan)? {
            Some(current) => current,
            None => {
                let initial = plan.initial_journal_document();
                self.publish_locked(plan, None, &initial)?;
                self.read_current(plan)?.ok_or_else(recovery_required)?
            }
        };
        if current.document.journal().completed() {
            return Ok(Ok(DisposableMacosServiceLifecycleDisposition::Settled {
                document: current.document,
            }));
        }

        let executing_index = current
            .document
            .journal()
            .records
            .iter()
            .position(|record| record.outcome == crate::journal::ActionOutcome::Executing);
        let (action_index, prepared) = if let Some(index) = executing_index {
            let action = action_kind(plan, index)?;
            match driver.recover(plan, action) {
                Ok(DisposableMacosServiceActionRecovery::Completed) => {
                    let successor = plan
                        .complete_executing_lifecycle_action(&current.document)
                        .map_err(|_| recovery_required())?;
                    self.publish_locked(plan, Some(&current), &successor)?;
                    return Ok(Ok(
                        DisposableMacosServiceLifecycleDisposition::ActionCompleted {
                            action,
                            document: successor,
                        },
                    ));
                }
                Ok(DisposableMacosServiceActionRecovery::Unknown) => {
                    return Ok(Ok(
                        DisposableMacosServiceLifecycleDisposition::RecoveryRequired {
                            action,
                            document: current.document,
                        },
                    ));
                }
                Ok(DisposableMacosServiceActionRecovery::RetryAuthorized) => {}
                Err(error) => return Ok(Err(error)),
            }
            let prepared = match driver.prepare(plan, action) {
                Ok(prepared) => prepared,
                Err(error) => return Ok(Err(error)),
            };
            (index, prepared)
        } else {
            let index = current
                .document
                .journal()
                .records
                .iter()
                .position(|record| record.outcome == crate::journal::ActionOutcome::Pending)
                .ok_or_else(recovery_required)?;
            let action = action_kind(plan, index)?;
            let prepared = match driver.prepare(plan, action) {
                Ok(prepared) => prepared,
                Err(error) => return Ok(Err(error)),
            };
            let successor = plan
                .begin_next_lifecycle_action(&current.document)
                .map_err(|_| recovery_required())?;
            self.publish_locked(plan, Some(&current), &successor)?;
            current = self.read_current(plan)?.ok_or_else(recovery_required)?;
            if current.document != successor {
                return Err(recovery_required());
            }
            (index, prepared)
        };

        let action = action_kind(plan, action_index)?;
        if let Err(error) = driver.execute(plan, action, prepared) {
            return Ok(Err(error));
        }
        match driver.confirm_completed(plan, action) {
            Err(error) => Ok(Err(error)),
            Ok(DisposableMacosServiceActionConfirmation::Unknown) => Ok(Ok(
                DisposableMacosServiceLifecycleDisposition::RecoveryRequired {
                    action,
                    document: current.document,
                },
            )),
            Ok(DisposableMacosServiceActionConfirmation::Completed) => {
                let successor = plan
                    .complete_executing_lifecycle_action(&current.document)
                    .map_err(|_| recovery_required())?;
                self.publish_locked(plan, Some(&current), &successor)?;
                Ok(Ok(
                    DisposableMacosServiceLifecycleDisposition::ActionCompleted {
                        action,
                        document: successor,
                    },
                ))
            }
        }
    }

    /// Publish the exact `pending -> executing` successor for the next lifecycle action.
    fn begin_next_action(
        &self,
        plan: &DisposableMacosServicePlan,
    ) -> Result<JournalStateDocument, DisposableMacosServiceLifecycleError> {
        let _lock = self.acquire_lock()?;
        self.synchronize_directory()?;
        let current = self.recover_locked(plan)?.ok_or_else(|| {
            lifecycle_error(
                DisposableMacosServiceLifecycleErrorKind::Missing,
                "disposable_service_lifecycle_document_missing",
            )
        })?;
        let successor = plan
            .begin_next_lifecycle_action(&current.document)
            .map_err(|_| recovery_required())?;
        self.publish_locked(plan, Some(&current), &successor)?;
        self.read_current(plan)?
            .map(|current| current.document)
            .ok_or_else(recovery_required)
    }

    /// Publish the exact `executing -> completed` successor after action-specific observation.
    fn complete_executing_action(
        &self,
        plan: &DisposableMacosServicePlan,
    ) -> Result<JournalStateDocument, DisposableMacosServiceLifecycleError> {
        let _lock = self.acquire_lock()?;
        self.synchronize_directory()?;
        let current = self.recover_locked(plan)?.ok_or_else(|| {
            lifecycle_error(
                DisposableMacosServiceLifecycleErrorKind::Missing,
                "disposable_service_lifecycle_document_missing",
            )
        })?;
        let successor = plan
            .complete_executing_lifecycle_action(&current.document)
            .map_err(|_| recovery_required())?;
        self.publish_locked(plan, Some(&current), &successor)?;
        self.read_current(plan)?
            .map(|current| current.document)
            .ok_or_else(recovery_required)
    }

    /// Load only a clean, current, exact lifecycle journal.
    pub(crate) fn load(
        &self,
        plan: &DisposableMacosServicePlan,
    ) -> Result<Option<JournalStateDocument>, DisposableMacosServiceLifecycleError> {
        let _lock = self.acquire_lock()?;
        self.synchronize_directory()?;
        if self
            .read_named(plan, PREPARED_LIFECYCLE_DOCUMENT)?
            .is_some()
        {
            return Err(recovery_required());
        }
        self.read_current(plan)
            .map(|document| document.map(|document| document.document))
    }

    fn acquire_lock(&self) -> Result<LifecycleLock, DisposableMacosServiceLifecycleError> {
        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let file = fs::openat(
            &self.directory,
            LOCK_DOCUMENT,
            flags,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| unsafe_state())?;
        self.synchronize_directory()?;
        let before = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_private_file(&before, self.owner, Some(0))?;
        let path_before = fs::statat(&self.directory, LOCK_DOCUMENT, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unsafe_state())?;
        if !same_file(&before, &path_before) {
            return Err(unsafe_state());
        }
        fs::flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == rustix::io::Errno::AGAIN {
                lifecycle_error(
                    DisposableMacosServiceLifecycleErrorKind::Busy,
                    "disposable_service_lifecycle_busy",
                )
            } else {
                io_error()
            }
        })?;
        let guard = LifecycleLock { file };
        let after = fs::fstat(&guard.file).map_err(|_| unsafe_state())?;
        let path_after = fs::statat(&self.directory, LOCK_DOCUMENT, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unsafe_state())?;
        if !same_file(&before, &after) || !same_file(&before, &path_after) {
            return Err(unsafe_state());
        }
        Ok(guard)
    }

    fn recover_locked(
        &self,
        plan: &DisposableMacosServicePlan,
    ) -> Result<Option<ExactDocument>, DisposableMacosServiceLifecycleError> {
        let current = self.read_current(plan)?;
        let Some(mut prepared) = self.read_named(plan, PREPARED_LIFECYCLE_DOCUMENT)? else {
            return Ok(current);
        };
        let valid = match &current {
            None => prepared.document == plan.initial_journal_document(),
            Some(current) => {
                prepared.document == current.document
                    || plan
                        .begin_next_lifecycle_action(&current.document)
                        .is_ok_and(|expected| expected == prepared.document)
                    || plan
                        .complete_executing_lifecycle_action(&current.document)
                        .is_ok_and(|expected| expected == prepared.document)
            }
        };
        if !valid {
            return Err(recovery_required());
        }
        self.revalidate_document(PREPARED_LIFECYCLE_DOCUMENT, &mut prepared)?;
        prepared.file.sync_all().map_err(|_| io_error())?;
        if let Some(current) = &current {
            self.revalidate_document(
                LIFECYCLE_DOCUMENT,
                &mut ExactDocument {
                    document: current.document.clone(),
                    file: current.file.try_clone().map_err(|_| io_error())?,
                    snapshot: current.snapshot,
                },
            )?;
        }
        fs::renameat_with(
            &self.directory,
            PREPARED_LIFECYCLE_DOCUMENT,
            &self.directory,
            LIFECYCLE_DOCUMENT,
            if current.is_none() {
                RenameFlags::NOREPLACE
            } else {
                RenameFlags::empty()
            },
        )
        .map_err(|_| recovery_required())?;
        self.synchronize_directory()?;
        self.read_current(plan)
    }

    fn publish_locked(
        &self,
        plan: &DisposableMacosServicePlan,
        current: Option<&ExactDocument>,
        successor: &JournalStateDocument,
    ) -> Result<(), DisposableMacosServiceLifecycleError> {
        if plan.validate_lifecycle_journal(successor).is_err() {
            return Err(recovery_required());
        }
        if self
            .read_named(plan, PREPARED_LIFECYCLE_DOCUMENT)?
            .is_some()
        {
            return Err(recovery_required());
        }
        let bytes = encode_journal_document(successor)
            .map_err(|_| recovery_required())?
            .into_bytes();
        if u64::try_from(bytes.len())
            .ok()
            .is_none_or(|len| len > MAX_LIFECYCLE_BYTES)
        {
            return Err(recovery_required());
        }
        let (stage_name, mut stage) = self.create_random_stage()?;
        stage.write_all(&bytes).map_err(|_| recovery_required())?;
        stage.sync_all().map_err(|_| recovery_required())?;
        stage
            .seek(SeekFrom::Start(0))
            .map_err(|_| recovery_required())?;
        if read_bounded(&mut stage)? != bytes {
            return Err(recovery_required());
        }
        stage
            .seek(SeekFrom::Start(0))
            .map_err(|_| recovery_required())?;
        if read_bounded(&mut stage)? != bytes {
            return Err(recovery_required());
        }
        let snapshot = FileSnapshot::from_stat(&fs::fstat(&stage).map_err(|_| unsafe_state())?);
        inspect_snapshot(&snapshot, self.owner, Some(bytes.len()))?;
        let path = fs::statat(&self.directory, &stage_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unsafe_state())?;
        if snapshot != FileSnapshot::from_stat(&path) {
            return Err(unsafe_state());
        }
        fs::renameat_with(
            &self.directory,
            &stage_name,
            &self.directory,
            PREPARED_LIFECYCLE_DOCUMENT,
            RenameFlags::NOREPLACE,
        )
        .map_err(|_| recovery_required())?;
        self.synchronize_directory()?;
        let mut prepared = self
            .read_named(plan, PREPARED_LIFECYCLE_DOCUMENT)?
            .ok_or_else(recovery_required)?;
        if prepared.document != *successor {
            return Err(recovery_required());
        }
        prepared.file.sync_all().map_err(|_| recovery_required())?;
        self.revalidate_document(PREPARED_LIFECYCLE_DOCUMENT, &mut prepared)?;
        if let Some(current) = current {
            let mut retained = ExactDocument {
                document: current.document.clone(),
                file: current.file.try_clone().map_err(|_| io_error())?,
                snapshot: current.snapshot,
            };
            self.revalidate_document(LIFECYCLE_DOCUMENT, &mut retained)?;
        }
        fs::renameat_with(
            &self.directory,
            PREPARED_LIFECYCLE_DOCUMENT,
            &self.directory,
            LIFECYCLE_DOCUMENT,
            if current.is_none() {
                RenameFlags::NOREPLACE
            } else {
                RenameFlags::empty()
            },
        )
        .map_err(|_| recovery_required())?;
        self.synchronize_directory()?;
        let published = self.read_current(plan)?.ok_or_else(recovery_required)?;
        if published.document != *successor {
            return Err(recovery_required());
        }
        Ok(())
    }

    fn create_random_stage(&self) -> Result<(String, File), DisposableMacosServiceLifecycleError> {
        let mut random_source = File::open(RANDOM_SOURCE).map_err(|_| io_error())?;
        for _ in 0..8 {
            let mut random = [0_u8; 16];
            random_source
                .read_exact(&mut random)
                .map_err(|_| io_error())?;
            let mut suffix = String::with_capacity(32);
            for byte in random {
                use std::fmt::Write as _;
                write!(&mut suffix, "{byte:02x}").expect("writing into String cannot fail");
            }
            let name = format!("{STAGE_PREFIX}{suffix}");
            let flags =
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            match fs::openat(&self.directory, &name, flags, Mode::from_raw_mode(0o600)) {
                Ok(file) => return Ok((name, File::from(file))),
                Err(rustix::io::Errno::EXIST) => {}
                Err(_) => return Err(io_error()),
            }
        }
        Err(io_error())
    }

    fn read_current(
        &self,
        plan: &DisposableMacosServicePlan,
    ) -> Result<Option<ExactDocument>, DisposableMacosServiceLifecycleError> {
        self.read_named(plan, LIFECYCLE_DOCUMENT)
    }

    fn read_named(
        &self,
        plan: &DisposableMacosServicePlan,
        name: &str,
    ) -> Result<Option<ExactDocument>, DisposableMacosServiceLifecycleError> {
        let held = match fs::openat(&self.directory, name, READ_FLAGS, Mode::empty()) {
            Ok(held) => held,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unsafe_state()),
        };
        let mut file = File::from(held);
        let before = fs::fstat(&file).map_err(|_| unsafe_state())?;
        inspect_private_file(&before, self.owner, None)?;
        let snapshot = FileSnapshot::from_stat(&before);
        let path_before = fs::statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unsafe_state())?;
        if snapshot != FileSnapshot::from_stat(&path_before) {
            return Err(unsafe_state());
        }
        let bytes = read_bounded(&mut file)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| unsafe_state())?;
        if read_bounded(&mut file)? != bytes {
            return Err(unsafe_state());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| recovery_required())?;
        let document = decode_journal_document(text).map_err(|_| recovery_required())?;
        if plan.validate_lifecycle_journal(&document).is_err()
            || encode_journal_document(&document)
                .map_err(|_| recovery_required())?
                .as_bytes()
                != bytes
        {
            return Err(recovery_required());
        }
        let mut exact = ExactDocument {
            document,
            file,
            snapshot,
        };
        self.revalidate_document(name, &mut exact)?;
        Ok(Some(exact))
    }

    fn revalidate_document(
        &self,
        name: &str,
        document: &mut ExactDocument,
    ) -> Result<(), DisposableMacosServiceLifecycleError> {
        let held = FileSnapshot::from_stat(&fs::fstat(&document.file).map_err(|_| unsafe_state())?);
        let path = FileSnapshot::from_stat(
            &fs::statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| unsafe_state())?,
        );
        if held != document.snapshot || path != document.snapshot {
            return Err(unsafe_state());
        }
        let directory = fs::fstat(&self.directory).map_err(|_| unsafe_state())?;
        inspect_directory(&directory, self.owner)
    }

    fn synchronize_directory(&self) -> Result<(), DisposableMacosServiceLifecycleError> {
        fs::fsync(&self.directory).map_err(|_| io_error())?;
        let held = fs::fstat(&self.directory).map_err(|_| unsafe_state())?;
        inspect_directory(&held, self.owner)?;
        let resolved = fs::stat(&self.path).map_err(|_| unsafe_state())?;
        if DirectoryIdentity::from_stat(&held) != self.identity
            || DirectoryIdentity::from_stat(&resolved) != self.identity
        {
            return Err(unsafe_state());
        }
        Ok(())
    }
}

fn action_kind(
    plan: &DisposableMacosServicePlan,
    index: usize,
) -> Result<DisposableMacosServiceActionKind, DisposableMacosServiceLifecycleError> {
    plan.report()
        .actions()
        .get(index)
        .map(|action| action.kind())
        .ok_or_else(recovery_required)
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>, DisposableMacosServiceLifecycleError> {
    let mut bytes = Vec::new();
    file.take(MAX_LIFECYCLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_state())?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|length| length > MAX_LIFECYCLE_BYTES)
    {
        return Err(recovery_required());
    }
    Ok(bytes)
}

fn inspect_directory(
    stat: &Stat,
    owner: (u32, u32),
) -> Result<(), DisposableMacosServiceLifecycleError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7777 != 0o700
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn inspect_private_file(
    stat: &Stat,
    owner: (u32, u32),
    expected_size: Option<usize>,
) -> Result<(), DisposableMacosServiceLifecycleError> {
    let snapshot = FileSnapshot::from_stat(stat);
    inspect_snapshot(&snapshot, owner, expected_size)
}

fn inspect_snapshot(
    snapshot: &FileSnapshot,
    owner: (u32, u32),
    expected_size: Option<usize>,
) -> Result<(), DisposableMacosServiceLifecycleError> {
    if snapshot.owner != owner.0
        || snapshot.group != owner.1
        || snapshot.mode & 0o170000 != 0o100000
        || snapshot.links != 1
        || snapshot.mode & 0o7777 != 0o600
        || snapshot.size < 0
        || u64::try_from(snapshot.size)
            .ok()
            .is_none_or(|size| size > MAX_LIFECYCLE_BYTES)
        || expected_size
            .is_some_and(|expected| usize::try_from(snapshot.size).ok() != Some(expected))
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn same_file(left: &Stat, right: &Stat) -> bool {
    FileSnapshot::from_stat(left) == FileSnapshot::from_stat(right)
}

fn same_directory(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
}

const fn lifecycle_error(
    kind: DisposableMacosServiceLifecycleErrorKind,
    code: &'static str,
) -> DisposableMacosServiceLifecycleError {
    DisposableMacosServiceLifecycleError { kind, code }
}

const fn unsafe_state() -> DisposableMacosServiceLifecycleError {
    lifecycle_error(
        DisposableMacosServiceLifecycleErrorKind::UnsafeState,
        "disposable_service_lifecycle_unsafe_state",
    )
}

const fn recovery_required() -> DisposableMacosServiceLifecycleError {
    lifecycle_error(
        DisposableMacosServiceLifecycleErrorKind::RecoveryRequired,
        "disposable_service_lifecycle_recovery_required",
    )
}

const fn io_error() -> DisposableMacosServiceLifecycleError {
    lifecycle_error(
        DisposableMacosServiceLifecycleErrorKind::Io,
        "disposable_service_lifecycle_io_failed",
    )
}

#[cfg(test)]
mod tests {
    use std::fs::{self as std_fs, OpenOptions};
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::artifact::Sha256Digest;
    use crate::disposable_macos_service_installation::{
        DisposableMacosServiceDesiredState, plan_disposable_macos_service,
    };
    use crate::journal::ActionOutcome;
    use crate::state::InstallationId;

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NETWORK_DIGEST: &str =
        "sha256:65ceec8974086e378f216acc555724cb40b08ccc047391dedd0b6f17df72587e";

    struct Fixture {
        root: PathBuf,
        parent: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let parent = PathBuf::from("target/disposable-service-lifecycle-fixtures");
            std_fs::create_dir_all(&parent).unwrap();
            for _ in 0..32 {
                let root = parent.join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
                ));
                match std_fs::create_dir(&root) {
                    Ok(()) => {
                        std_fs::set_permissions(&root, std_fs::Permissions::from_mode(0o700))
                            .unwrap();
                        return Self { root, parent };
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create fixture: {error}"),
                }
            }
            panic!("could not allocate lifecycle fixture")
        }

        fn owner(&self) -> (u32, u32) {
            let metadata = std_fs::metadata(&self.root).unwrap();
            (metadata.uid(), metadata.gid())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std_fs::remove_dir_all(&self.root);
            let _ = std_fs::remove_dir(&self.parent);
        }
    }

    fn plan() -> DisposableMacosServicePlan {
        let enrollment = format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 2,\n",
                "  \"state_root\": \"/private/var/lib/smolrunner\",\n",
                "  \"network\": {{\n",
                "    \"backend\": \"macos_pf_dedicated_uid\",\n",
                "    \"service_uid\": 502,\n",
                "    \"policy_identity\": \"{NETWORK_DIGEST}\"\n",
                "  }},\n",
                "  \"lima\": {{\n",
                "    \"program\": \"/opt/homebrew/bin/limactl\",\n",
                "    \"home\": \"/private/var/lib/smolrunner/lima\",\n",
                "    \"source_instance\": \"smolrunner-prepared-template\"\n",
                "  }},\n",
                "  \"bridge\": {{\n",
                "    \"program_digest\": \"{DIGEST_B}\"\n",
                "  }},\n",
                "  \"github\": {{\n",
                "    \"config_url\": \"https://github.com/acme\",\n",
                "    \"client_id\": \"Iv1.0123456789abcdef\",\n",
                "    \"installation_id\": 42,\n",
                "    \"keychain_service\": \"smolrunner.github-app\",\n",
                "    \"keychain_account\": \"acme-ci\"\n",
                "  }},\n",
                "  \"scale_set\": {{\n",
                "    \"id\": 17,\n",
                "    \"name\": \"smolrunner-disposable\",\n",
                "    \"runner_group_id\": 3,\n",
                "    \"owner\": \"acme\",\n",
                "    \"repository\": \"widgets\",\n",
                "    \"labels\": [\n",
                "      \"self-hosted\",\n",
                "      \"smolrunner\"\n",
                "    ]\n",
                "  }},\n",
                "  \"resources\": {{\n",
                "    \"cpu_millis\": 2000,\n",
                "    \"memory_bytes\": 2147483648,\n",
                "    \"disk_bytes\": 21474836480\n",
                "  }}\n",
                "}}\n"
            ),
            NETWORK_DIGEST = NETWORK_DIGEST,
            DIGEST_B = DIGEST_B,
        )
        .into_bytes();
        plan_disposable_macos_service(
            DisposableMacosServiceDesiredState::Installed,
            &InstallationId::parse("smolrunner-install-0001").unwrap(),
            Path::new("/opt/operator/smolrunner"),
            &Sha256Digest::parse(DIGEST_A).unwrap(),
            Path::new("/opt/operator/scaleset-bridge"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            Path::new("/opt/operator/enrollment.json"),
            &enrollment,
        )
        .unwrap()
    }

    struct FakeDriver {
        lock: PathBuf,
        recovery: DisposableMacosServiceActionRecovery,
        confirmation: DisposableMacosServiceActionConfirmation,
        fail_execute: bool,
        prepared: usize,
        executed: usize,
    }

    impl FakeDriver {
        fn new(root: &Path) -> Self {
            Self {
                lock: root.join(LOCK_DOCUMENT),
                recovery: DisposableMacosServiceActionRecovery::Unknown,
                confirmation: DisposableMacosServiceActionConfirmation::Completed,
                fail_execute: false,
                prepared: 0,
                executed: 0,
            }
        }

        fn require_transaction_lock(&self) {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.lock)
                .unwrap();
            assert_eq!(
                fs::flock(lock.as_fd(), FlockOperation::NonBlockingLockExclusive).unwrap_err(),
                rustix::io::Errno::AGAIN
            );
        }
    }

    impl DisposableMacosServiceActionDriver for FakeDriver {
        type Prepared = DisposableMacosServiceActionKind;
        type Error = &'static str;

        fn recover(
            &mut self,
            _plan: &DisposableMacosServicePlan,
            _action: DisposableMacosServiceActionKind,
        ) -> Result<DisposableMacosServiceActionRecovery, Self::Error> {
            self.require_transaction_lock();
            Ok(self.recovery)
        }

        fn prepare(
            &mut self,
            _plan: &DisposableMacosServicePlan,
            action: DisposableMacosServiceActionKind,
        ) -> Result<Self::Prepared, Self::Error> {
            self.require_transaction_lock();
            self.prepared += 1;
            Ok(action)
        }

        fn execute(
            &mut self,
            _plan: &DisposableMacosServicePlan,
            action: DisposableMacosServiceActionKind,
            prepared: Self::Prepared,
        ) -> Result<(), Self::Error> {
            self.require_transaction_lock();
            assert_eq!(prepared, action);
            self.executed += 1;
            if self.fail_execute {
                Err("ambiguous execute failure")
            } else {
                Ok(())
            }
        }

        fn confirm_completed(
            &mut self,
            _plan: &DisposableMacosServicePlan,
            _action: DisposableMacosServiceActionKind,
        ) -> Result<DisposableMacosServiceActionConfirmation, Self::Error> {
            self.require_transaction_lock();
            Ok(self.confirmation)
        }
    }

    #[test]
    fn initializes_and_advances_only_exact_contiguous_states() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let plan = plan();
        let initial = store.initialize_or_recover(&plan).unwrap();
        assert_eq!(initial, plan.initial_journal_document());

        let executing = store.begin_next_action(&plan).unwrap();
        assert_eq!(
            executing.journal().records[1].outcome,
            ActionOutcome::Executing
        );
        assert_eq!(
            executing.journal().records[2].outcome,
            ActionOutcome::Pending
        );
        assert_eq!(
            store.begin_next_action(&plan).unwrap_err().kind(),
            DisposableMacosServiceLifecycleErrorKind::RecoveryRequired
        );

        let completed = store.complete_executing_action(&plan).unwrap();
        assert_eq!(
            completed.journal().records[1].outcome,
            ActionOutcome::Completed
        );
        assert_eq!(
            completed.journal().records[2].outcome,
            ActionOutcome::Pending
        );
        assert_eq!(store.load(&plan).unwrap(), Some(completed));
    }

    #[test]
    fn canonical_prepared_successor_is_recovered_after_publish_interruption() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let plan = plan();
        let initial = store.initialize_or_recover(&plan).unwrap();
        let successor = plan.begin_next_lifecycle_action(&initial).unwrap();
        let bytes = encode_journal_document(&successor).unwrap();
        let prepared = fixture.root.join(PREPARED_LIFECYCLE_DOCUMENT);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&prepared)
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
        file.sync_all().unwrap();

        let recovered = store.initialize_or_recover(&plan).unwrap();
        assert_eq!(recovered, successor);
        assert!(!prepared.exists());
    }

    #[test]
    fn partial_or_foreign_prepared_bytes_are_preserved_and_refused() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let plan = plan();
        store.initialize_or_recover(&plan).unwrap();
        let prepared = fixture.root.join(PREPARED_LIFECYCLE_DOCUMENT);
        std_fs::write(&prepared, b"partial").unwrap();
        std_fs::set_permissions(&prepared, std_fs::Permissions::from_mode(0o600)).unwrap();

        let error = store.initialize_or_recover(&plan).unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableMacosServiceLifecycleErrorKind::RecoveryRequired
        );
        assert_eq!(std_fs::read(prepared).unwrap(), b"partial");
    }

    #[test]
    fn noncanonical_current_document_is_refused() {
        let fixture = Fixture::new();
        let plan = plan();
        let current = fixture.root.join(LIFECYCLE_DOCUMENT);
        let canonical = encode_journal_document(&plan.initial_journal_document()).unwrap();
        std_fs::write(&current, canonical.trim_end().as_bytes()).unwrap();
        std_fs::set_permissions(&current, std_fs::Permissions::from_mode(0o600)).unwrap();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();

        assert_eq!(
            store.initialize_or_recover(&plan).unwrap_err().kind(),
            DisposableMacosServiceLifecycleErrorKind::RecoveryRequired
        );
    }

    #[test]
    fn directory_path_rebind_is_refused_before_publication() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let detached = fixture.root.with_extension("detached");
        std_fs::rename(&fixture.root, &detached).unwrap();
        std_fs::create_dir(&fixture.root).unwrap();
        std_fs::set_permissions(&fixture.root, std_fs::Permissions::from_mode(0o700)).unwrap();

        let error = store.initialize_or_recover(&plan()).unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableMacosServiceLifecycleErrorKind::UnsafeState
        );
        assert!(!fixture.root.join(LIFECYCLE_DOCUMENT).exists());
        assert!(!detached.join(LIFECYCLE_DOCUMENT).exists());

        std_fs::remove_dir(&fixture.root).unwrap();
        std_fs::rename(detached, &fixture.root).unwrap();
    }

    #[test]
    fn one_action_transaction_holds_lock_across_checkpoint_execute_and_confirmation() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let plan = plan();
        let mut driver = FakeDriver::new(&fixture.root);

        let disposition = store.reconcile_one(&plan, &mut driver).unwrap().unwrap();
        let DisposableMacosServiceLifecycleDisposition::ActionCompleted { action, document } =
            disposition
        else {
            panic!("expected one completed action")
        };
        assert_eq!(
            action,
            DisposableMacosServiceActionKind::EnsureServiceAccount
        );
        assert_eq!(driver.prepared, 1);
        assert_eq!(driver.executed, 1);
        assert_eq!(
            document.journal().records[1].outcome,
            ActionOutcome::Completed
        );
        assert_eq!(
            document.journal().records[2].outcome,
            ActionOutcome::Pending
        );
    }

    #[test]
    fn ambiguous_failure_remains_executing_until_exact_recovery_authorizes_retry() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let plan = plan();
        let mut failing = FakeDriver::new(&fixture.root);
        failing.fail_execute = true;
        assert_eq!(
            store.reconcile_one(&plan, &mut failing).unwrap(),
            Err("ambiguous execute failure")
        );
        let executing = store.load(&plan).unwrap().unwrap();
        assert_eq!(
            executing.journal().records[1].outcome,
            ActionOutcome::Executing
        );

        let mut unknown = FakeDriver::new(&fixture.root);
        let disposition = store.reconcile_one(&plan, &mut unknown).unwrap().unwrap();
        assert!(matches!(
            disposition,
            DisposableMacosServiceLifecycleDisposition::RecoveryRequired {
                action: DisposableMacosServiceActionKind::EnsureServiceAccount,
                ..
            }
        ));
        assert_eq!(unknown.prepared, 0);
        assert_eq!(unknown.executed, 0);

        let mut retry = FakeDriver::new(&fixture.root);
        retry.recovery = DisposableMacosServiceActionRecovery::RetryAuthorized;
        let disposition = store.reconcile_one(&plan, &mut retry).unwrap().unwrap();
        assert!(matches!(
            disposition,
            DisposableMacosServiceLifecycleDisposition::ActionCompleted {
                action: DisposableMacosServiceActionKind::EnsureServiceAccount,
                ..
            }
        ));
        assert_eq!(retry.executed, 1);
    }

    #[test]
    fn observed_completed_recovery_advances_without_reexecution() {
        let fixture = Fixture::new();
        let store =
            DisposableMacosServiceLifecycleStore::open_existing(&fixture.root, fixture.owner())
                .unwrap();
        let plan = plan();
        let mut failing = FakeDriver::new(&fixture.root);
        failing.fail_execute = true;
        let _ = store.reconcile_one(&plan, &mut failing).unwrap();

        let mut recovered = FakeDriver::new(&fixture.root);
        recovered.recovery = DisposableMacosServiceActionRecovery::Completed;
        let disposition = store.reconcile_one(&plan, &mut recovered).unwrap().unwrap();
        assert!(matches!(
            disposition,
            DisposableMacosServiceLifecycleDisposition::ActionCompleted { .. }
        ));
        assert_eq!(recovered.prepared, 0);
        assert_eq!(recovered.executed, 0);
    }
}
