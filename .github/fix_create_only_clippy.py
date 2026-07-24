from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise RuntimeError(
            f"expected exactly one match in {path}, found {count}: {old[:120]!r}"
        )
    file.write_text(text.replace(old, new, 1))


replace_once(
    "src/state_store.rs",
    """            if self.entries.contains_key(&key) {
                return Err(StateStoreError::public(
                    super::StateStoreErrorKind::Conflict,
                    "state destination already exists",
                ));
            }
            self.entries.insert(key, record.bytes().to_vec());
            Ok(StateWriteReceipt::new(
                StateWriteDisposition::Created,
                record.bytes().len(),
            ))""",
    """            match self.entries.entry(key) {
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
            }""",
)

replace_once(
    "src/durable_journal.rs",
    """            if self.entries.contains_key(&key) {
                return Err(StateStoreError::public(
                    StateStoreErrorKind::Conflict,
                    "state destination already exists",
                ));
            }
            self.entries.insert(key, record.bytes().to_vec());
            Ok(StateWriteReceipt::new(
                StateWriteDisposition::Created,
                record.bytes().len(),
            ))""",
    """            match self.entries.entry(key) {
                std::collections::btree_map::Entry::Occupied(_) => Err(StateStoreError::public(
                    StateStoreErrorKind::Conflict,
                    "state destination already exists",
                )),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(record.bytes().to_vec());
                    Ok(StateWriteReceipt::new(
                        StateWriteDisposition::Created,
                        record.bytes().len(),
                    ))
                }
            }""",
)
