from pathlib import Path

path = Path("src/linux_state.rs")
text = path.read_text(encoding="utf-8")

old = '''#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePhase {
    BeforeWrite,
    BeforeFileSync,
    BeforeRename,
    BeforeParentSync,
}
'''
new = '''#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePhase {
    BeforeWrite,
    BeforeFileSync,
    BeforeRename,
    BeforeParentSync,
}

trait WriteFaultInjector {
    fn check(&mut self, phase: WritePhase) -> Result<(), StateStoreError>;
}

struct NoWriteFaults;

impl WriteFaultInjector for NoWriteFaults {
    fn check(&mut self, _phase: WritePhase) -> Result<(), StateStoreError> {
        Ok(())
    }
}
'''
if text.count(old) != 1:
    raise SystemExit("unexpected write phase block")
text = text.replace(old, new, 1)

old = '''        let mut hook = |_phase| Ok::<(), StateStoreError>(());
        self.write_atomic_with_hook(record, &mut hook)
'''
new = '''        let mut faults = NoWriteFaults;
        self.write_atomic_with_faults(record, &mut faults)
'''
if text.count(old) != 1:
    raise SystemExit("unexpected production hook block")
text = text.replace(old, new, 1)

old = '''    fn write_atomic_with_hook<F>(
        &mut self,
        record: &StateRecord,
        hook: &mut F,
    ) -> Result<StateWriteReceipt, StateStoreError>
    where
        F: FnMut(WritePhase) -> Result<(), StateStoreError>,
    {
'''
new = '''    fn write_atomic_with_faults(
        &mut self,
        record: &StateRecord,
        faults: &mut dyn WriteFaultInjector,
    ) -> Result<StateWriteReceipt, StateStoreError> {
'''
if text.count(old) != 1:
    raise SystemExit("unexpected atomic hook signature")
text = text.replace(old, new, 1)
text = text.replace("write_and_sync(temporary, record.bytes(), hook)?;", "write_and_sync(temporary, record.bytes(), faults)?;", 1)
text = text.replace("hook(WritePhase::BeforeRename)?;", "faults.check(WritePhase::BeforeRename)?;", 1)
text = text.replace("hook(WritePhase::BeforeParentSync)?;", "faults.check(WritePhase::BeforeParentSync)?;", 1)

old = '''fn write_and_sync<F>(fd: OwnedFd, bytes: &[u8], hook: &mut F) -> Result<(), StateStoreError>
where
    F: FnMut(WritePhase) -> Result<(), StateStoreError>,
{
    let mut file = File::from(fd);
    hook(WritePhase::BeforeWrite)?;
'''
new = '''fn write_and_sync(
    fd: OwnedFd,
    bytes: &[u8],
    faults: &mut dyn WriteFaultInjector,
) -> Result<(), StateStoreError> {
    let mut file = File::from(fd);
    faults.check(WritePhase::BeforeWrite)?;
'''
if text.count(old) != 1:
    raise SystemExit("unexpected write-and-sync hook signature")
text = text.replace(old, new, 1)
text = text.replace("hook(WritePhase::BeforeFileSync)?;", "faults.check(WritePhase::BeforeFileSync)?;", 1)

old = "    use super::{LOCK_FILE_NAME, LinuxStateRoot, TEMP_FILE_PREFIX, WritePhase};"
new = '''    use super::{
        LOCK_FILE_NAME, LinuxStateRoot, TEMP_FILE_PREFIX, WriteFailurePoint,
        WriteFaultInjector,
    };'''
# Keep the enum name WritePhase in this implementation; only expand the import.
new = '''    use super::{
        LOCK_FILE_NAME, LinuxStateRoot, TEMP_FILE_PREFIX, WriteFaultInjector, WritePhase,
    };'''
if text.count(old) != 1:
    raise SystemExit("unexpected test import")
text = text.replace(old, new, 1)

start = text.index("    fn fail_at(")
end = text.index("    fn assert_no_temporary_files", start)
replacement = '''    struct FailAt(WritePhase);

    impl WriteFaultInjector for FailAt {
        fn check(&mut self, phase: WritePhase) -> Result<(), StateStoreError> {
            if phase != self.0 {
                return Ok(());
            }
            let message = match phase {
                WritePhase::BeforeWrite => {
                    "injected state-write failure before temporary-file write"
                }
                WritePhase::BeforeFileSync => {
                    "injected state-write failure before temporary-file synchronization"
                }
                WritePhase::BeforeRename => {
                    "injected state-write failure before publication rename"
                }
                WritePhase::BeforeParentSync => {
                    "state file was published before an injected parent-sync failure"
                }
            };
            Err(StateStoreError::public(StateStoreErrorKind::Io, message))
        }
    }

'''
text = text[:start] + replacement + text[end:]
text = text.replace("let mut hook = fail_at(point);", "let mut faults = FailAt(point);", 1)
text = text.replace(".write_atomic_with_hook(&replacement, &mut hook)", ".write_atomic_with_faults(&replacement, &mut faults)", 1)
text = text.replace(
    "let mut hook = fail_at(WritePhase::BeforeParentSync);",
    "let mut faults = FailAt(WritePhase::BeforeParentSync);",
    1,
)
text = text.replace(".write_atomic_with_hook(&replacement, &mut hook)", ".write_atomic_with_faults(&replacement, &mut faults)", 1)

if "write_atomic_with_hook" in text or "let mut hook" in text:
    raise SystemExit("generic hook references remain")
path.write_text(text, encoding="utf-8")
