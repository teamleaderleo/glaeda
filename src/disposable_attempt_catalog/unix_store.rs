use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogError,
    DisposableAttemptCatalogErrorKind, DisposableAttemptCatalogRevision,
    DisposableAttemptCatalogStore, DisposableAttemptCatalogWriteDisposition,
    DisposableAttemptCatalogWriteReceipt, MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
    catalog_error, decode_disposable_attempt_catalog, encode_disposable_attempt_catalog,
};

pub const DISPOSABLE_ATTEMPT_CATALOG_STATE_FILE: &str = "disposable-attempts.json";
pub const DISPOSABLE_ATTEMPT_CATALOG_STAGED_FILE: &str = "disposable-attempts.next";
pub const DISPOSABLE_ATTEMPT_CATALOG_LOCK_FILE: &str = "disposable-attempts.lock";

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Crash-safe Unix persistence for the host-global disposable-attempt catalog.
///
/// The caller supplies one already-created private controller state root. This adapter owns only
/// three fixed files beneath it and never creates, removes, or chmods the state root itself.
#[derive(Debug, Clone)]
pub struct UnixDisposableAttemptCatalogStore {
    root: PathBuf,
}

impl UnixDisposableAttemptCatalogStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state_path(&self) -> PathBuf {
        self.root.join(DISPOSABLE_ATTEMPT_CATALOG_STATE_FILE)
    }

    fn staged_path(&self) -> PathBuf {
        self.root.join(DISPOSABLE_ATTEMPT_CATALOG_STAGED_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(DISPOSABLE_ATTEMPT_CATALOG_LOCK_FILE)
    }

    fn validate_root(&self) -> Result<u32, DisposableAttemptCatalogError> {
        let metadata = fs::symlink_metadata(&self.root).map_err(storage_io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(corrupt_store(
                "disposable attempt catalog root must be a real directory",
            ));
        }
        if metadata.mode() & 0o777 != PRIVATE_DIRECTORY_MODE {
            return Err(corrupt_store(
                "disposable attempt catalog root must have private directory permissions",
            ));
        }
        Ok(metadata.uid())
    }

    fn open_lock(&self) -> Result<File, DisposableAttemptCatalogError> {
        let expected_uid = self.validate_root()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.lock_path())
            .map_err(storage_io)?;
        validate_private_file(&file, expected_uid, "lock")?;
        file.try_lock().map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                catalog_error(
                    DisposableAttemptCatalogErrorKind::Conflict,
                    "disposable attempt catalog store is busy",
                )
            } else {
                storage_io(error)
            }
        })?;
        Ok(file)
    }

    fn read_optional(
        &self,
        path: &Path,
        expected_uid: u32,
    ) -> Result<Option<Vec<u8>>, DisposableAttemptCatalogError> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_io(error)),
        };
        validate_private_file(&file, expected_uid, "state")?;
        let metadata = file.metadata().map_err(storage_io)?;
        if metadata.len() > MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES as u64 {
            return Err(corrupt_store(
                "disposable attempt catalog file exceeds the reviewed byte limit",
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes).map_err(storage_io)?;
        if bytes.len() > MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES {
            return Err(corrupt_store(
                "disposable attempt catalog file exceeds the reviewed byte limit",
            ));
        }
        Ok(Some(bytes))
    }

    fn recover_locked(
        &self,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError> {
        let expected_uid = self.validate_root()?;
        let state_path = self.state_path();
        let staged_path = self.staged_path();
        let main_bytes = self.read_optional(&state_path, expected_uid)?;
        let staged_bytes = self.read_optional(&staged_path, expected_uid)?;

        let main = main_bytes
            .as_deref()
            .map(decode_store_document)
            .transpose()?;
        let staged = match staged_bytes.as_deref() {
            Some(bytes) => match decode_store_document(bytes) {
                Ok(document) => Some(document),
                Err(error) => {
                    if main.is_some() || main_bytes.is_none() {
                        remove_staged_if_present(&staged_path)?;
                        sync_directory(&self.root)?;
                        return Ok(main);
                    }
                    return Err(error);
                }
            },
            None => None,
        };

        match (main, staged) {
            (None, None) => Ok(None),
            (Some(current), None) => Ok(Some(current)),
            (None, Some(next)) => {
                if next.revision().get() != 1 {
                    return Err(corrupt_store(
                        "orphan staged disposable attempt catalog is not an initial revision",
                    ));
                }
                publish_staged(&staged_path, &state_path, &self.root)?;
                Ok(Some(next))
            }
            (Some(current), Some(next)) => {
                if next.revision().get() == current.revision().get() {
                    remove_staged_if_present(&staged_path)?;
                    sync_directory(&self.root)?;
                    return Ok(Some(current));
                }
                let expected_next = current.revision().get().checked_add(1).ok_or_else(|| {
                    corrupt_store("disposable attempt catalog revision cannot advance")
                })?;
                if next.revision().get() != expected_next {
                    return Err(corrupt_store(
                        "staged disposable attempt catalog revision is inconsistent with current state",
                    ));
                }
                publish_staged(&staged_path, &state_path, &self.root)?;
                Ok(Some(next))
            }
        }
    }

    fn write_staged(
        &self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<(), DisposableAttemptCatalogError> {
        let expected_uid = self.validate_root()?;
        let encoded = encode_disposable_attempt_catalog(document).map_err(|_| {
            corrupt_store("disposable attempt catalog cannot be canonically encoded")
        })?;
        let staged_path = self.staged_path();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staged_path)
            .map_err(storage_io)?;
        validate_private_file(&file, expected_uid, "staged state")?;
        file.write_all(&encoded).map_err(storage_io)?;
        file.sync_all().map_err(storage_io)?;
        drop(file);
        Ok(())
    }

    fn publish(
        &self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<(), DisposableAttemptCatalogError> {
        self.write_staged(document)?;
        publish_staged(&self.staged_path(), &self.state_path(), &self.root)
    }
}

impl DisposableAttemptCatalogStore for UnixDisposableAttemptCatalogStore {
    fn load(
        &self,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError> {
        let _lock = self.open_lock()?;
        self.recover_locked()
    }

    fn create(
        &mut self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        if document.revision().get() != 1 {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::Conflict,
                "initial disposable attempt catalog must use revision one",
            ));
        }
        let _lock = self.open_lock()?;
        if self.recover_locked()?.is_some() {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::AlreadyExists,
                "disposable attempt catalog already exists",
            ));
        }
        self.publish(document)?;
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Created,
            document.revision(),
            None,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableAttemptCatalogRevision,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        let _lock = self.open_lock()?;
        let current = self.recover_locked()?.ok_or_else(|| {
            catalog_error(
                DisposableAttemptCatalogErrorKind::Missing,
                "disposable attempt catalog is not initialized",
            )
        })?;
        if current.revision() != expected_revision {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::Conflict,
                "stale disposable attempt catalog revision",
            ));
        }
        let required_revision = expected_revision.get().checked_add(1).ok_or_else(|| {
            catalog_error(
                DisposableAttemptCatalogErrorKind::Conflict,
                "disposable attempt catalog revision cannot advance",
            )
        })?;
        if document.revision().get() != required_revision {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::Conflict,
                "replacement disposable attempt catalog must advance exactly one revision",
            ));
        }
        self.publish(document)?;
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Replaced,
            document.revision(),
            None,
        ))
    }
}

fn validate_private_file(
    file: &File,
    expected_uid: u32,
    _role: &'static str,
) -> Result<(), DisposableAttemptCatalogError> {
    let metadata = file.metadata().map_err(storage_io)?;
    if !metadata.is_file()
        || metadata.uid() != expected_uid
        || metadata.mode() & 0o777 != PRIVATE_FILE_MODE
        || metadata.nlink() != 1
    {
        return Err(corrupt_store(
            "disposable attempt catalog file metadata is unsafe",
        ));
    }
    Ok(())
}

fn decode_store_document(
    bytes: &[u8],
) -> Result<DisposableAttemptCatalogDocument, DisposableAttemptCatalogError> {
    decode_disposable_attempt_catalog(bytes).map_err(|_| {
        corrupt_store("disposable attempt catalog file contains invalid canonical state")
    })
}

fn publish_staged(
    staged_path: &Path,
    state_path: &Path,
    root: &Path,
) -> Result<(), DisposableAttemptCatalogError> {
    fs::rename(staged_path, state_path).map_err(storage_io)?;
    sync_directory(root)
}

fn remove_staged_if_present(path: &Path) -> Result<(), DisposableAttemptCatalogError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_io(error)),
    }
}

fn sync_directory(path: &Path) -> Result<(), DisposableAttemptCatalogError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(storage_io)
}

fn storage_io(_error: io::Error) -> DisposableAttemptCatalogError {
    catalog_error(
        DisposableAttemptCatalogErrorKind::CorruptState,
        "disposable attempt catalog filesystem operation failed",
    )
}

fn corrupt_store(message: &'static str) -> DisposableAttemptCatalogError {
    catalog_error(DisposableAttemptCatalogErrorKind::CorruptState, message)
}
