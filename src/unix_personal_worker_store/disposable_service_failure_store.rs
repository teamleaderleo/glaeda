use std::fs::File;
use std::io::{Read as _, Seek as _};

use super::*;
use crate::disposable_service_failure_receipt::{
    DisposableServiceFailureReceipt, DisposableServiceFailureReceiptErrorKind,
};

pub(super) const FAILURE_RECEIPT_DOCUMENT: &str = "disposable-service-failure.json";
pub(super) const STAGED_FAILURE_RECEIPT_DOCUMENT: &str = ".disposable-service-failure.next.json";
const MAX_STORED_FAILURE_RECEIPT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableServiceFailureStoreRecoveryReason {
    StagedCandidate,
    AmbiguousReplacement,
    DuplicateStage,
    CorruptCurrent,
    CorruptStaged,
    VersionIncompatibleCurrent,
    VersionIncompatibleStaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisposableServiceFailureStoreInspection {
    Missing,
    Current(DisposableServiceFailureReceipt),
    RecoveryRequired {
        reason: DisposableServiceFailureStoreRecoveryReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableServiceFailureStoreRecoveryDisposition {
    Clean,
    RemovedDuplicateStaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableServiceFailureStoreWriteDisposition {
    Created,
    Replaced,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableServiceFailureStoreClearDisposition {
    Cleared,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiptSlot {
    Current,
    Staged,
}

impl ReceiptSlot {
    const fn name(self) -> &'static str {
        match self {
            Self::Current => FAILURE_RECEIPT_DOCUMENT,
            Self::Staged => STAGED_FAILURE_RECEIPT_DOCUMENT,
        }
    }

    const fn subject(self) -> &'static str {
        match self {
            Self::Current => "disposable service failure receipt",
            Self::Staged => "staged disposable service failure receipt",
        }
    }

    const fn corrupt_reason(self) -> DisposableServiceFailureStoreRecoveryReason {
        match self {
            Self::Current => DisposableServiceFailureStoreRecoveryReason::CorruptCurrent,
            Self::Staged => DisposableServiceFailureStoreRecoveryReason::CorruptStaged,
        }
    }

    const fn version_reason(self) -> DisposableServiceFailureStoreRecoveryReason {
        match self {
            Self::Current => {
                DisposableServiceFailureStoreRecoveryReason::VersionIncompatibleCurrent
            }
            Self::Staged => {
                DisposableServiceFailureStoreRecoveryReason::VersionIncompatibleStaged
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceiptFileSnapshot {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    size: i64,
}

impl ReceiptFileSnapshot {
    #[allow(clippy::useless_conversion)]
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: u64::try_from(stat.st_dev).unwrap_or(u64::MAX),
            inode: stat.st_ino,
            owner: stat.st_uid,
            group: stat.st_gid,
            mode: u32::from(stat.st_mode),
            links: u64::from(stat.st_nlink),
            size: stat.st_size,
        }
    }
}

struct ExactReceiptFile {
    slot: ReceiptSlot,
    file: File,
    receipt: DisposableServiceFailureReceipt,
    snapshot: ReceiptFileSnapshot,
}

enum RetainedReceipt {
    Missing,
    Valid(ExactReceiptFile),
    Invalid(DisposableServiceFailureStoreRecoveryReason),
}

enum RecoveryPlan {
    CleanMissing,
    CleanCurrent(ExactReceiptFile),
    RemoveDuplicateStaged {
        current: ExactReceiptFile,
        staged: ExactReceiptFile,
    },
    RecoveryRequired(DisposableServiceFailureStoreRecoveryReason),
}

struct RecoveredState {
    disposition: DisposableServiceFailureStoreRecoveryDisposition,
    current: Option<ExactReceiptFile>,
}

impl UnixPersonalWorkerStore {
    /// Inspect retained failure evidence without publishing, deleting, or repairing it.
    pub(crate) fn inspect_disposable_service_failure_store(
        &self,
    ) -> Result<DisposableServiceFailureStoreInspection, PersonalWorkerStoreError> {
        let _lock = self.acquire_read_lock()?;
        match self.failure_receipt_recovery_plan()? {
            RecoveryPlan::CleanMissing => Ok(DisposableServiceFailureStoreInspection::Missing),
            RecoveryPlan::CleanCurrent(current) => {
                Ok(DisposableServiceFailureStoreInspection::Current(current.receipt))
            }
            RecoveryPlan::RemoveDuplicateStaged { .. } => Ok(
                DisposableServiceFailureStoreInspection::RecoveryRequired {
                    reason: DisposableServiceFailureStoreRecoveryReason::DuplicateStage,
                },
            ),
            RecoveryPlan::RecoveryRequired(reason) => {
                Ok(DisposableServiceFailureStoreInspection::RecoveryRequired { reason })
            }
        }
    }

    /// Reconcile only recovery that can be proven exact from the retained receipt pair.
    ///
    /// A stage with no current receipt, or a stage different from the current receipt, stays
    /// untouched. The raw receipt has no predecessor transaction identity, so adopting such a
    /// stage by filename alone would turn an ambiguous same-user file into durable evidence.
    pub(crate) fn recover_disposable_service_failure_store(
        &mut self,
    ) -> Result<DisposableServiceFailureStoreRecoveryDisposition, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_failure_receipt_directory(self)?;
        Ok(self.recover_failure_receipt_locked()?.disposition)
    }

    /// Atomically publish one already-validated bounded diagnostic receipt.
    pub(crate) fn replace_disposable_service_failure_receipt(
        &mut self,
        receipt: &DisposableServiceFailureReceipt,
    ) -> Result<DisposableServiceFailureStoreWriteDisposition, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_failure_receipt_directory(self)?;
        let recovered = self.recover_failure_receipt_locked()?;
        if recovered
            .current
            .as_ref()
            .is_some_and(|current| current.receipt == *receipt)
        {
            return Ok(DisposableServiceFailureStoreWriteDisposition::Duplicate);
        }

        let disposition = if recovered.current.is_some() {
            DisposableServiceFailureStoreWriteDisposition::Replaced
        } else {
            DisposableServiceFailureStoreWriteDisposition::Created
        };
        let encoded = receipt.canonical_json();
        if encoded.len() > MAX_STORED_FAILURE_RECEIPT_BYTES {
            return Err(corrupt_store());
        }
        let mut staged = self.stage_named_bytes(STAGED_FAILURE_RECEIPT_DOCUMENT, &encoded)?;
        if let Some(mut current) = recovered.current {
            self.confirm_exact_failure_receipt(&mut current)?;
        }
        self.publish_named_staged(
            &mut staged,
            FAILURE_RECEIPT_DOCUMENT,
            disposition == DisposableServiceFailureStoreWriteDisposition::Created,
        )?;
        Ok(disposition)
    }

    /// Remove only the exact canonical current receipt after closing proven recovery debt.
    pub(crate) fn clear_disposable_service_failure_receipt(
        &mut self,
    ) -> Result<DisposableServiceFailureStoreClearDisposition, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_failure_receipt_directory(self)?;
        let recovered = self.recover_failure_receipt_locked()?;
        let Some(mut current) = recovered.current else {
            return Ok(DisposableServiceFailureStoreClearDisposition::Missing);
        };
        self.confirm_exact_failure_receipt(&mut current)?;
        self.remove_exact_failure_receipt(current)?;
        Ok(DisposableServiceFailureStoreClearDisposition::Cleared)
    }

    fn failure_receipt_recovery_plan(&self) -> Result<RecoveryPlan, PersonalWorkerStoreError> {
        let current = self.read_failure_receipt(ReceiptSlot::Current)?;
        let staged = self.read_failure_receipt(ReceiptSlot::Staged)?;

        match (current, staged) {
            (RetainedReceipt::Missing, RetainedReceipt::Missing) => Ok(RecoveryPlan::CleanMissing),
            (RetainedReceipt::Valid(current), RetainedReceipt::Missing) => {
                Ok(RecoveryPlan::CleanCurrent(current))
            }
            (RetainedReceipt::Invalid(reason), _) => Ok(RecoveryPlan::RecoveryRequired(reason)),
            (_, RetainedReceipt::Invalid(reason)) => Ok(RecoveryPlan::RecoveryRequired(reason)),
            (RetainedReceipt::Missing, RetainedReceipt::Valid(_)) => {
                Ok(RecoveryPlan::RecoveryRequired(
                    DisposableServiceFailureStoreRecoveryReason::StagedCandidate,
                ))
            }
            (RetainedReceipt::Valid(current), RetainedReceipt::Valid(staged))
                if current.receipt == staged.receipt =>
            {
                Ok(RecoveryPlan::RemoveDuplicateStaged { current, staged })
            }
            (RetainedReceipt::Valid(_), RetainedReceipt::Valid(_)) => {
                Ok(RecoveryPlan::RecoveryRequired(
                    DisposableServiceFailureStoreRecoveryReason::AmbiguousReplacement,
                ))
            }
        }
    }

    fn recover_failure_receipt_locked(
        &self,
    ) -> Result<RecoveredState, PersonalWorkerStoreError> {
        match self.failure_receipt_recovery_plan()? {
            RecoveryPlan::CleanMissing => Ok(RecoveredState {
                disposition: DisposableServiceFailureStoreRecoveryDisposition::Clean,
                current: None,
            }),
            RecoveryPlan::CleanCurrent(mut current) => {
                self.confirm_exact_failure_receipt(&mut current)?;
                Ok(RecoveredState {
                    disposition: DisposableServiceFailureStoreRecoveryDisposition::Clean,
                    current: Some(current),
                })
            }
            RecoveryPlan::RemoveDuplicateStaged {
                mut current,
                mut staged,
            } => {
                self.confirm_exact_failure_receipt(&mut current)?;
                self.confirm_exact_failure_receipt(&mut staged)?;
                self.remove_exact_failure_receipt(staged)?;
                Ok(RecoveredState {
                    disposition:
                        DisposableServiceFailureStoreRecoveryDisposition::RemovedDuplicateStaged,
                    current: Some(current),
                })
            }
            RecoveryPlan::RecoveryRequired(reason) => Err(recovery_error(reason)),
        }
    }

    fn read_failure_receipt(
        &self,
        slot: ReceiptSlot,
    ) -> Result<RetainedReceipt, PersonalWorkerStoreError> {
        inspect_directory(
            &self.directory,
            "personal worker store directory",
            Some(self.owner),
        )?;
        let file = match fs::openat(&self.directory, slot.name(), EXISTING_FILE_FLAGS, Mode::empty()) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(RetainedReceipt::Missing),
            Err(error) => return Err(map_document_open_error(error)),
        };
        inspect_private_file(&file, self.owner, slot.subject(), None)?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_STORED_FAILURE_RECEIPT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| io_store("could not read the disposable service failure receipt"))?;
        if bytes.len() > MAX_STORED_FAILURE_RECEIPT_BYTES {
            return Ok(RetainedReceipt::Invalid(slot.corrupt_reason()));
        }
        let receipt = match DisposableServiceFailureReceipt::from_json(&bytes) {
            Ok(receipt) => receipt,
            Err(error)
                if error.kind()
                    == DisposableServiceFailureReceiptErrorKind::UnsupportedSchema =>
            {
                return Ok(RetainedReceipt::Invalid(slot.version_reason()));
            }
            Err(_) => return Ok(RetainedReceipt::Invalid(slot.corrupt_reason())),
        };
        if receipt.canonical_json() != bytes {
            return Ok(RetainedReceipt::Invalid(slot.corrupt_reason()));
        }
        let snapshot = ReceiptFileSnapshot::from_stat(
            &fs::fstat(file.as_fd())
                .map_err(|_| io_store("could not inspect the disposable service failure receipt"))?,
        );
        let path_stat = match fs::statat(&self.directory, slot.name(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => {
                return Err(conflict_store(
                    "disposable service failure receipt changed while it was opened",
                ));
            }
            Err(error) => return Err(map_document_open_error(error)),
        };
        if snapshot != ReceiptFileSnapshot::from_stat(&path_stat) {
            return Err(conflict_store(
                "disposable service failure receipt changed while it was opened",
            ));
        }
        Ok(RetainedReceipt::Valid(ExactReceiptFile {
            slot,
            file,
            receipt,
            snapshot,
        }))
    }

    fn confirm_exact_failure_receipt(
        &self,
        exact: &mut ExactReceiptFile,
    ) -> Result<(), PersonalWorkerStoreError> {
        exact
            .file
            .rewind()
            .map_err(|_| io_store("could not seek the disposable service failure receipt"))?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut exact.file)
            .take((MAX_STORED_FAILURE_RECEIPT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| io_store("could not reread the disposable service failure receipt"))?;
        if bytes.len() > MAX_STORED_FAILURE_RECEIPT_BYTES
            || exact.receipt.canonical_json() != bytes
            || DisposableServiceFailureReceipt::from_json(&bytes).as_ref() != Ok(&exact.receipt)
        {
            return Err(corrupt_store());
        }
        let descriptor_snapshot = ReceiptFileSnapshot::from_stat(
            &fs::fstat(exact.file.as_fd())
                .map_err(|_| io_store("could not inspect the disposable service failure receipt"))?,
        );
        if descriptor_snapshot != exact.snapshot {
            return Err(conflict_store(
                "disposable service failure receipt changed after validation",
            ));
        }
        let path_stat = match fs::statat(
            &self.directory,
            exact.slot.name(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => {
                return Err(conflict_store(
                    "disposable service failure receipt path changed after validation",
                ));
            }
            Err(error) => return Err(map_document_open_error(error)),
        };
        if ReceiptFileSnapshot::from_stat(&path_stat) != exact.snapshot {
            return Err(conflict_store(
                "disposable service failure receipt path changed after validation",
            ));
        }
        Ok(())
    }

    fn remove_exact_failure_receipt(
        &self,
        mut exact: ExactReceiptFile,
    ) -> Result<(), PersonalWorkerStoreError> {
        self.confirm_exact_failure_receipt(&mut exact)?;
        fs::unlinkat(&self.directory, exact.slot.name(), AtFlags::empty())
            .map_err(|_| io_store("could not remove the disposable service failure receipt"))?;
        synchronize_failure_receipt_directory(self)
    }
}

fn synchronize_failure_receipt_directory(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    // A writer may have renamed successfully and then observed an ambiguous parent-fsync failure.
    // Every later recovery/mutation closes that window under the same canonical lock first.
    synchronize_directory(&store.directory, "personal worker store directory")
}

fn recovery_error(
    reason: DisposableServiceFailureStoreRecoveryReason,
) -> PersonalWorkerStoreError {
    match reason {
        DisposableServiceFailureStoreRecoveryReason::VersionIncompatibleCurrent
        | DisposableServiceFailureStoreRecoveryReason::VersionIncompatibleStaged => store_error(
            PersonalWorkerStoreErrorKind::VersionIncompatible,
            "disposable service failure receipt uses an unsupported schema version",
        ),
        DisposableServiceFailureStoreRecoveryReason::CorruptCurrent
        | DisposableServiceFailureStoreRecoveryReason::CorruptStaged => corrupt_store(),
        DisposableServiceFailureStoreRecoveryReason::StagedCandidate => conflict_store(
            "staged disposable service failure receipt requires explicit recovery",
        ),
        DisposableServiceFailureStoreRecoveryReason::AmbiguousReplacement => conflict_store(
            "disposable service failure receipt replacement is ambiguous",
        ),
        DisposableServiceFailureStoreRecoveryReason::DuplicateStage => conflict_store(
            "duplicate disposable service failure receipt stage changed during recovery",
        ),
    }
}

fn corrupt_store() -> PersonalWorkerStoreError {
    store_error(
        PersonalWorkerStoreErrorKind::CorruptState,
        "disposable service failure receipt state is corrupt or noncanonical",
    )
}

fn conflict_store(message: &'static str) -> PersonalWorkerStoreError {
    store_error(PersonalWorkerStoreErrorKind::RevisionConflict, message)
}

fn io_store(message: &'static str) -> PersonalWorkerStoreError {
    store_error(PersonalWorkerStoreErrorKind::Io, message)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::disposable_service_failure_receipt::{
        DisposableServiceFailureCode, DisposableServiceFailureKind,
    };
    use crate::unix_personal_worker_store::publication_fault::{
        PublicationFaultPoint, inject_publication_fault,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "glaeda-service-failure-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set temporary root mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn store_path(&self, name: &str) -> PathBuf {
            self.0.join(STORE_DIRECTORY).join(name)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn receipt(generation: u64, code: &'static str) -> DisposableServiceFailureReceipt {
        DisposableServiceFailureReceipt::new(
            digest('1'),
            digest('2'),
            digest('3'),
            DisposableServiceFailureKind::Supervisor,
            DisposableServiceFailureCode::from_static(code).unwrap(),
            100,
            100 + generation,
            generation,
            generation % 2 == 0,
        )
        .unwrap()
    }

    fn open_store(root: &TempRoot) -> UnixPersonalWorkerStore {
        UnixPersonalWorkerStore::open_or_create_disposable_worker_service_store(root.path())
            .expect("open service store")
    }

    fn write_slot(root: &TempRoot, slot: ReceiptSlot, bytes: &[u8]) {
        let path = root.store_path(slot.name());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(&path).expect("create retained test receipt");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("set retained receipt mode");
        std::io::Write::write_all(&mut file, bytes).expect("write retained test receipt");
        file.sync_all().expect("sync retained test receipt");
        File::open(root.path().join(STORE_DIRECTORY))
            .expect("open store directory")
            .sync_all()
            .expect("sync store directory");
    }

    fn version_two_bytes() -> Vec<u8> {
        String::from_utf8(receipt(1, "failure").canonical_json())
            .unwrap()
            .replacen("\"schema_version\":1", "\"schema_version\":2", 1)
            .into_bytes()
    }

    #[test]
    fn missing_store_is_clean_and_survives_reopen() {
        let root = TempRoot::new("missing");
        let mut store = open_store(&root);
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::Missing
        );
        assert_eq!(
            store.recover_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreRecoveryDisposition::Clean
        );
        drop(store);
        let reopened = open_store(&root);
        assert_eq!(
            reopened.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::Missing
        );
    }

    #[test]
    fn replace_is_durable_idempotent_and_constant_space() {
        let root = TempRoot::new("replace");
        let mut store = open_store(&root);
        let first = receipt(1, "bridge_unavailable");
        assert_eq!(
            store.replace_disposable_service_failure_receipt(&first).unwrap(),
            DisposableServiceFailureStoreWriteDisposition::Created
        );
        assert_eq!(
            store.replace_disposable_service_failure_receipt(&first).unwrap(),
            DisposableServiceFailureStoreWriteDisposition::Duplicate
        );
        for generation in 2..=32 {
            let next = receipt(generation, "bridge_unavailable");
            assert_eq!(
                store.replace_disposable_service_failure_receipt(&next).unwrap(),
                DisposableServiceFailureStoreWriteDisposition::Replaced
            );
        }
        assert!(root.store_path(FAILURE_RECEIPT_DOCUMENT).is_file());
        assert!(!root.store_path(STAGED_FAILURE_RECEIPT_DOCUMENT).exists());
        let retained = fs::read(root.store_path(FAILURE_RECEIPT_DOCUMENT)).unwrap();
        assert_eq!(
            DisposableServiceFailureReceipt::from_json(&retained).unwrap(),
            receipt(32, "bridge_unavailable")
        );
        drop(store);
        let reopened = open_store(&root);
        assert_eq!(
            reopened.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::Current(receipt(32, "bridge_unavailable"))
        );
    }

    #[test]
    fn staged_candidate_without_current_is_preserved_as_recovery_debt() {
        let root = TempRoot::new("stage-create");
        let mut store = open_store(&root);
        let staged = receipt(4, "supervisor_failed");
        let bytes = staged.canonical_json();
        write_slot(&root, ReceiptSlot::Staged, &bytes);
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::RecoveryRequired {
                reason: DisposableServiceFailureStoreRecoveryReason::StagedCandidate
            }
        );
        let error = store
            .recover_disposable_service_failure_store()
            .expect_err("stage-only state lacks proof for automatic publication");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
        assert_eq!(
            fs::read(root.store_path(STAGED_FAILURE_RECEIPT_DOCUMENT)).unwrap(),
            bytes
        );
        assert!(!root.store_path(FAILURE_RECEIPT_DOCUMENT).exists());
    }

    #[test]
    fn differing_current_and_stage_are_preserved_as_ambiguous() {
        let root = TempRoot::new("stage-replace");
        let mut store = open_store(&root);
        let first = receipt(1, "first_failure");
        store.replace_disposable_service_failure_receipt(&first).unwrap();
        let second = receipt(2, "second_failure");
        let staged_bytes = second.canonical_json();
        write_slot(&root, ReceiptSlot::Staged, &staged_bytes);
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::RecoveryRequired {
                reason: DisposableServiceFailureStoreRecoveryReason::AmbiguousReplacement
            }
        );
        let error = store
            .recover_disposable_service_failure_store()
            .expect_err("different staged receipt must remain ambiguous");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
        assert_eq!(
            fs::read(root.store_path(FAILURE_RECEIPT_DOCUMENT)).unwrap(),
            first.canonical_json()
        );
        assert_eq!(
            fs::read(root.store_path(STAGED_FAILURE_RECEIPT_DOCUMENT)).unwrap(),
            staged_bytes
        );
    }

    #[test]
    fn exact_duplicate_stage_is_the_only_automatic_stage_cleanup() {
        let root = TempRoot::new("duplicate-stage");
        let mut store = open_store(&root);
        let current = receipt(2, "second_failure");
        store
            .replace_disposable_service_failure_receipt(&current)
            .unwrap();
        write_slot(&root, ReceiptSlot::Staged, &current.canonical_json());
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::RecoveryRequired {
                reason: DisposableServiceFailureStoreRecoveryReason::DuplicateStage
            }
        );
        assert_eq!(
            store.recover_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreRecoveryDisposition::RemovedDuplicateStaged
        );
        assert!(!root.store_path(STAGED_FAILURE_RECEIPT_DOCUMENT).exists());
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::Current(current)
        );
    }

    #[test]
    fn corrupt_and_version_incompatible_state_is_preserved_for_explicit_recovery() {
        for (label, slot, bytes, expected_reason, expected_kind) in [
            (
                "corrupt-current",
                ReceiptSlot::Current,
                b"{bad-json".to_vec(),
                DisposableServiceFailureStoreRecoveryReason::CorruptCurrent,
                PersonalWorkerStoreErrorKind::CorruptState,
            ),
            (
                "corrupt-stage",
                ReceiptSlot::Staged,
                b"{bad-json".to_vec(),
                DisposableServiceFailureStoreRecoveryReason::CorruptStaged,
                PersonalWorkerStoreErrorKind::CorruptState,
            ),
            (
                "version-current",
                ReceiptSlot::Current,
                version_two_bytes(),
                DisposableServiceFailureStoreRecoveryReason::VersionIncompatibleCurrent,
                PersonalWorkerStoreErrorKind::VersionIncompatible,
            ),
            (
                "version-stage",
                ReceiptSlot::Staged,
                version_two_bytes(),
                DisposableServiceFailureStoreRecoveryReason::VersionIncompatibleStaged,
                PersonalWorkerStoreErrorKind::VersionIncompatible,
            ),
        ] {
            let root = TempRoot::new(label);
            let mut store = open_store(&root);
            write_slot(&root, slot, &bytes);
            assert_eq!(
                store.inspect_disposable_service_failure_store().unwrap(),
                DisposableServiceFailureStoreInspection::RecoveryRequired {
                    reason: expected_reason
                }
            );
            let error = store
                .recover_disposable_service_failure_store()
                .expect_err("invalid retained state must stay recovery-required");
            assert_eq!(error.kind(), expected_kind);
            assert_eq!(fs::read(root.store_path(slot.name())).unwrap(), bytes);
        }
    }

    #[test]
    fn noncanonical_and_oversized_documents_are_preserved_as_corrupt() {
        for (label, bytes) in [
            (
                "noncanonical",
                {
                    let mut bytes = receipt(1, "failure").canonical_json();
                    bytes.push(b'\n');
                    bytes
                },
            ),
            (
                "oversized",
                vec![b'x'; MAX_STORED_FAILURE_RECEIPT_BYTES + 1],
            ),
        ] {
            let root = TempRoot::new(label);
            let store = open_store(&root);
            write_slot(&root, ReceiptSlot::Current, &bytes);
            assert_eq!(
                store.inspect_disposable_service_failure_store().unwrap(),
                DisposableServiceFailureStoreInspection::RecoveryRequired {
                    reason: DisposableServiceFailureStoreRecoveryReason::CorruptCurrent
                }
            );
            assert_eq!(
                fs::read(root.store_path(FAILURE_RECEIPT_DOCUMENT)).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn unsafe_file_identity_is_refused_without_cleanup() {
        let root = TempRoot::new("unsafe");
        let store = open_store(&root);
        let bytes = receipt(1, "failure").canonical_json();
        let target = root.path().join("foreign.json");
        fs::write(&target, &bytes).unwrap();
        symlink(&target, root.store_path(FAILURE_RECEIPT_DOCUMENT)).unwrap();
        let error = store
            .inspect_disposable_service_failure_store()
            .expect_err("symlinked receipt must be refused");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::UnsafeFilesystem);
        assert!(target.exists());
        assert!(root.store_path(FAILURE_RECEIPT_DOCUMENT).is_symlink());
    }

    #[test]
    fn wrong_mode_and_hardlink_are_refused_without_cleanup() {
        for label in ["mode", "hardlink"] {
            let root = TempRoot::new(label);
            let store = open_store(&root);
            write_slot(
                &root,
                ReceiptSlot::Current,
                &receipt(1, "failure").canonical_json(),
            );
            if label == "mode" {
                fs::set_permissions(
                    root.store_path(FAILURE_RECEIPT_DOCUMENT),
                    fs::Permissions::from_mode(0o640),
                )
                .unwrap();
            } else {
                fs::hard_link(
                    root.store_path(FAILURE_RECEIPT_DOCUMENT),
                    root.path().join("second-link.json"),
                )
                .unwrap();
            }
            let error = store
                .inspect_disposable_service_failure_store()
                .expect_err("unsafe receipt file must be refused");
            assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::UnsafeFilesystem);
            assert!(root.store_path(FAILURE_RECEIPT_DOCUMENT).exists());
        }
    }

    #[test]
    fn pre_rename_publication_faults_leave_the_previous_current_value() {
        for point in [
            PublicationFaultPoint::StageWrite,
            PublicationFaultPoint::StageFileSync,
            PublicationFaultPoint::PublishRename,
        ] {
            let root = TempRoot::new("pre-rename-fault");
            let mut store = open_store(&root);
            let first = receipt(1, "first_failure");
            store.replace_disposable_service_failure_receipt(&first).unwrap();
            let _fault = inject_publication_fault(point);
            let error = store
                .replace_disposable_service_failure_receipt(&receipt(2, "second_failure"))
                .expect_err("injected publication fault must fail the write");
            assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
            assert_eq!(
                store.inspect_disposable_service_failure_store().unwrap(),
                DisposableServiceFailureStoreInspection::Current(first)
            );
        }
    }

    #[test]
    fn directory_sync_fault_converges_to_the_renamed_candidate() {
        let root = TempRoot::new("post-rename-fault");
        let mut store = open_store(&root);
        let first = receipt(1, "first_failure");
        store.replace_disposable_service_failure_receipt(&first).unwrap();
        let second = receipt(2, "second_failure");
        let _fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
        let error = store
            .replace_disposable_service_failure_receipt(&second)
            .expect_err("directory sync fault must report ambiguous publication");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
        assert_eq!(
            store.recover_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreRecoveryDisposition::Clean
        );
        assert_eq!(
            store.replace_disposable_service_failure_receipt(&second).unwrap(),
            DisposableServiceFailureStoreWriteDisposition::Duplicate
        );
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::Current(second)
        );
    }

    #[test]
    fn clear_removes_only_exact_valid_current_state() {
        let root = TempRoot::new("clear");
        let mut store = open_store(&root);
        store
            .replace_disposable_service_failure_receipt(&receipt(1, "failure"))
            .unwrap();
        assert_eq!(
            store.clear_disposable_service_failure_receipt().unwrap(),
            DisposableServiceFailureStoreClearDisposition::Cleared
        );
        assert_eq!(
            store.clear_disposable_service_failure_receipt().unwrap(),
            DisposableServiceFailureStoreClearDisposition::Missing
        );
        assert_eq!(
            store.inspect_disposable_service_failure_store().unwrap(),
            DisposableServiceFailureStoreInspection::Missing
        );
    }

    #[test]
    fn ambiguous_stage_blocks_clear_and_replace_without_mutation() {
        let root = TempRoot::new("ambiguous-block");
        let mut store = open_store(&root);
        let current = receipt(1, "first_failure");
        store
            .replace_disposable_service_failure_receipt(&current)
            .unwrap();
        let staged = receipt(2, "second_failure");
        write_slot(&root, ReceiptSlot::Staged, &staged.canonical_json());
        assert_eq!(
            store
                .clear_disposable_service_failure_receipt()
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
        assert_eq!(
            store
                .replace_disposable_service_failure_receipt(&receipt(3, "third_failure"))
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
        assert_eq!(
            fs::read(root.store_path(FAILURE_RECEIPT_DOCUMENT)).unwrap(),
            current.canonical_json()
        );
        assert_eq!(
            fs::read(root.store_path(STAGED_FAILURE_RECEIPT_DOCUMENT)).unwrap(),
            staged.canonical_json()
        );
    }

    #[test]
    fn store_errors_remain_path_free() {
        let root = TempRoot::new("privacy");
        let store = open_store(&root);
        write_slot(&root, ReceiptSlot::Current, b"/private/operator/secret");
        let inspection = store.inspect_disposable_service_failure_store().unwrap();
        assert_eq!(
            inspection,
            DisposableServiceFailureStoreInspection::RecoveryRequired {
                reason: DisposableServiceFailureStoreRecoveryReason::CorruptCurrent
            }
        );
        assert!(!format!("{inspection:?}").contains(root.path().to_string_lossy().as_ref()));
    }
}
