from pathlib import Path

source = Path("src/rootless_podman_config_observation.rs")
text = source.read_text()

old_flags = '''const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
'''
new_flags = '''const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
'''
if text.count(old_flags) != 1:
    raise SystemExit("final-file flags anchor missing or duplicated")
text = text.replace(old_flags, new_flags, 1)

old_owner = '''        ExpectedOwner::Root => metadata.uid == 0,
'''
new_owner = '''        ExpectedOwner::Root => metadata.uid == 0 && metadata.gid == 0,
'''
if text.count(old_owner) != 1:
    raise SystemExit("root ownership anchor missing or duplicated")
text = text.replace(old_owner, new_owner, 1)
source.write_text(text)

tests = Path("src/rootless_podman_config_observation/tests.rs")
text = tests.read_text()

old_imports = '''use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::sync::atomic::{AtomicU64, Ordering};
'''
new_imports = '''use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
'''
if text.count(old_imports) != 1:
    raise SystemExit("test import anchor missing or duplicated")
text = text.replace(old_imports, new_imports, 1)

root_anchor = '''    assert!(validate_file_metadata(root_file, ExpectedOwner::Root).is_ok());
'''
root_test = root_anchor + '''    assert_eq!(
        validate_file_metadata(
            ConfigMetadata {
                gid: 1,
                ..root_file
            },
            ExpectedOwner::Root,
        ),
        Err(RootlessPodmanConfigSourceProblemKind::WrongOwner)
    );
'''
if text.count(root_anchor) != 1:
    raise SystemExit("root ownership test anchor missing or duplicated")
text = text.replace(root_anchor, root_test, 1)

fifo_anchor = '''    fs::remove_dir(&source).expect("remove source directory");

    symlink("/etc/passwd", &source).expect("create final symlink");
'''
fifo_test = '''    fs::remove_dir(&source).expect("remove source directory");

    let status = Command::new("/usr/bin/mkfifo")
        .arg(&source)
        .status()
        .expect("create FIFO source");
    assert!(status.success());
    let (sender, receiver) = mpsc::channel();
    let fifo_source = source.clone();
    thread::spawn(move || {
        let _ = sender.send(read_linux_config(&fifo_source, expected_owner));
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("FIFO observation must not block waiting for a writer"),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::NonRegularFile)
    );
    fs::remove_file(&source).expect("remove FIFO source");

    symlink("/etc/passwd", &source).expect("create final symlink");
'''
if text.count(fifo_anchor) != 1:
    raise SystemExit("FIFO test anchor missing or duplicated")
text = text.replace(fifo_anchor, fifo_test, 1)
tests.write_text(text)
