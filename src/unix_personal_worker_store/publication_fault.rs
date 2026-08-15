use std::cell::Cell;

use crate::personal_worker_store::{PersonalWorkerStoreError, PersonalWorkerStoreErrorKind};

use super::store_error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationFaultPoint {
    StageWrite,
    StageFileSync,
    PublishRename,
    PublicationDirectorySync,
}

thread_local! {
    static PUBLICATION_FAULT: Cell<Option<PublicationFaultPoint>> = const { Cell::new(None) };
}

pub(super) struct PublicationFaultGuard;

pub(super) fn inject_publication_fault(point: PublicationFaultPoint) -> PublicationFaultGuard {
    PUBLICATION_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "publication fault already armed"
        );
    });
    PublicationFaultGuard
}

pub(super) fn maybe_fail_publication(
    point: PublicationFaultPoint,
) -> Result<(), PersonalWorkerStoreError> {
    let injected = PUBLICATION_FAULT.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            true
        } else {
            false
        }
    });
    if injected {
        Err(store_error(
            PersonalWorkerStoreErrorKind::Io,
            "injected personal worker durable publication failure",
        ))
    } else {
        Ok(())
    }
}

impl Drop for PublicationFaultGuard {
    fn drop(&mut self) {
        PUBLICATION_FAULT.with(|slot| slot.set(None));
    }
}
