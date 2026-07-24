from pathlib import Path

path = Path("src/linux_state.rs")
text = path.read_text(encoding="utf-8")

old = '''pub struct LinuxStateRoot {
    root: OwnedFd,
}
'''
new = '''pub struct LinuxStateRoot {
    root: OwnedFd,
    owner: (u32, u32),
}
'''
if text.count(old) != 1:
    raise SystemExit("unexpected LinuxStateRoot definition")
text = text.replace(old, new, 1)

old = '''    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateStoreError> {
        let root =
            fs::open(path.as_ref(), DIRECTORY_FLAGS, Mode::empty()).map_err(map_root_open_error)?;
        verify_directory(&root, "state root")?;
        Ok(Self { root })
    }
'''
new = '''    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateStoreError> {
        let root =
            fs::open(path.as_ref(), DIRECTORY_FLAGS, Mode::empty()).map_err(map_root_open_error)?;
        let stat = verify_managed_directory(&root, "state root", None)?;
        Ok(Self {
            root,
            owner: (stat.st_uid, stat.st_gid),
        })
    }
'''
if text.count(old) != 1:
    raise SystemExit("unexpected LinuxStateRoot::open")
text = text.replace(old, new, 1)

text = text.replace(
    'verify_directory(&directory, "state path parent")?;',
    'verify_managed_directory(&directory, "state path parent", Some(self.owner))?;',
)
text = text.replace(
    'verify_directory(&current, "state path parent")?;',
    'verify_managed_directory(&current, "state path parent", Some(self.owner))?;',
)
text = text.replace(
    'verify_directory(&installations, "installations directory")?;',
    'verify_managed_directory(&installations, "installations directory", Some(self.owner))?;',
)
text = text.replace(
    'verify_directory(&installation, "installation directory")?;',
    'verify_managed_directory(&installation, "installation directory", Some(self.owner))?;',
)

old = '        verify_regular_file(&file, "state file", true)?;\n'
new = '        verify_managed_file(&file, "state file", self.owner, true)?;\n'
if text.count(old) != 1:
    raise SystemExit("unexpected state-file read verification")
text = text.replace(old, new, 1)

old = '        let disposition = inspect_destination(&parent, file_name)?;\n'
new = '        let disposition = inspect_destination(&parent, file_name, self.owner)?;\n'
if text.count(old) != 1:
    raise SystemExit("unexpected destination inspection call")
text = text.replace(old, new, 1)

old = '''        fs::fchmod(&temporary, PRIVATE_FILE_MODE).map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "could not set private state-file permissions",
            )
        })?;
        write_and_sync(temporary, record.bytes(), faults)?;
'''
new = '''        fs::fchmod(&temporary, PRIVATE_FILE_MODE).map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "could not set private state-file permissions",
            )
        })?;
        verify_managed_file(&temporary, "temporary state file", self.owner, false)?;
        write_and_sync(temporary, record.bytes(), faults)?;
'''
if text.count(old) != 1:
    raise SystemExit("unexpected temporary-file preparation")
text = text.replace(old, new, 1)

old = '        let lock = open_installation_lock(&installation)?;\n'
new = '        let lock = open_installation_lock(&installation, self.owner)?;\n'
if text.count(old) != 1:
    raise SystemExit("unexpected installation-lock call")
text = text.replace(old, new, 1)

start = text.index("fn open_installation_lock(")
end = text.index("\nfn inspect_destination(", start)
replacement = '''fn open_installation_lock(
    installation: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, StateStoreError> {
    match fs::openat(
        installation,
        LOCK_FILE_NAME,
        NEW_LOCK_FLAGS,
        PRIVATE_FILE_MODE,
    ) {
        Ok(lock) => {
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                StateStoreError::public(
                    StateStoreErrorKind::Io,
                    "could not set installation-lock permissions",
                )
            })?;
            verify_lock_file(&lock, owner)?;
            Ok(lock)
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(
                installation,
                LOCK_FILE_NAME,
                EXISTING_LOCK_FLAGS,
                Mode::empty(),
            )
            .map_err(map_lock_open_error)?;
            verify_lock_file(&lock, owner)?;
            Ok(lock)
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}
'''
text = text[:start] + replacement + text[end:]

start = text.index("fn inspect_destination(")
end = text.index("\nfn create_temporary_file", start)
replacement = '''fn inspect_destination(
    parent: &OwnedFd,
    file_name: &StateComponent,
    owner: (u32, u32),
) -> Result<StateWriteDisposition, StateStoreError> {
    match fs::openat(parent, file_name.as_str(), FILE_FLAGS, Mode::empty()) {
        Ok(file) => {
            verify_managed_file(&file, "existing state file", owner, true)?;
            Ok(StateWriteDisposition::Replaced)
        }
        Err(Errno::NOENT) => Ok(StateWriteDisposition::Created),
        Err(error) => Err(map_component_open_error(error)),
    }
}
'''
text = text[:start] + replacement + text[end:]

start = text.index("fn verify_directory(")
end = text.index("\nfn read_bounded", start)
replacement = '''fn verify_managed_directory(
    fd: &OwnedFd,
    subject: &str,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, StateStoreError> {
    let stat = fs::fstat(fd).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a directory"),
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0750"),
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    Ok(stat)
}

fn verify_managed_file(
    fd: &OwnedFd,
    subject: &str,
    owner: (u32, u32),
    enforce_size_limit: bool,
) -> Result<rustix::fs::Stat, StateStoreError> {
    let stat = fs::fstat(fd).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a regular file"),
        ));
    }
    if stat.st_nlink != 1 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has multiple hard links"),
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0600"),
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    if enforce_size_limit
        && (stat.st_size < 0 || stat.st_size as u64 > MAX_STATE_DOCUMENT_BYTES as u64)
    {
        return Err(StateStoreError::public(
            StateStoreErrorKind::CorruptState,
            format!("{subject} exceeds the configured size limit"),
        ));
    }
    Ok(stat)
}

fn verify_lock_file(fd: &OwnedFd, owner: (u32, u32)) -> Result<(), StateStoreError> {
    let stat = verify_managed_file(fd, "installation lock", owner, false)?;
    if stat.st_size == 0 {
        Ok(())
    } else {
        Err(StateStoreError::public(
            StateStoreErrorKind::CorruptState,
            "installation lock contains unexpected data",
        ))
    }
}
'''
text = text[:start] + replacement + text[end:]

old = '''            fs::create_dir(&path).expect("create isolated temporary root");
            Self { path }
'''
new = '''            fs::create_dir(&path).expect("create isolated temporary root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set state-root mode");
            Self { path }
'''
if text.count(old) != 1:
    raise SystemExit("unexpected TempTree creation")
text = text.replace(old, new, 1)

old = '''    fn create_project_parent(root: &Path) -> PathBuf {
        let installation = root.join("installations").join(installation_id().as_str());
        fs::create_dir_all(&installation).expect("create project parent");
        installation
    }
'''
new = '''    fn create_project_parent(root: &Path) -> PathBuf {
        let installations = root.join("installations");
        let installation = installations.join(installation_id().as_str());
        fs::create_dir_all(&installation).expect("create project parent");
        for directory in [&installations, &installation] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o750))
                .expect("set managed directory mode");
        }
        installation
    }
'''
if text.count(old) != 1:
    raise SystemExit("unexpected project-parent helper")
text = text.replace(old, new, 1)

old = '''        fs::write(parent.join("project.json"), b"{\\"schema_version\\":1}\\n")
            .expect("write project state");

        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
'''
new = '''        let project_path = parent.join("project.json");
        fs::write(&project_path, b"{\\"schema_version\\":1}\\n")
            .expect("write project state");
        fs::set_permissions(&project_path, fs::Permissions::from_mode(0o600))
            .expect("set project-state mode");

        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
'''
if text.count(old) != 1:
    raise SystemExit("unexpected regular-read fixture")
text = text.replace(old, new, 1)

old = '''        fs::write(
            parent.join("project.json"),
            vec![0_u8; MAX_STATE_DOCUMENT_BYTES + 1],
        )
        .expect("write oversized state");
        let error = reader
'''
new = '''        let oversized = parent.join("project.json");
        fs::write(&oversized, vec![0_u8; MAX_STATE_DOCUMENT_BYTES + 1])
            .expect("write oversized state");
        fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600))
            .expect("set oversized state mode");
        let error = reader
'''
if text.count(old) != 1:
    raise SystemExit("unexpected oversized fixture")
text = text.replace(old, new, 1)

marker = "    #[test]\n    fn symlinked_root_is_rejected() {\n"
tests = '''    #[test]
    fn broad_root_parent_or_state_file_is_rejected() {
        let root = TempTree::new("broad-managed-state");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))
            .expect("broaden state root");
        let error = LinuxStateRoot::open(root.path()).expect_err("broad root must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o750))
            .expect("restore state-root mode");

        let parent = create_project_parent(root.path());
        let installations = root.path().join("installations");
        fs::set_permissions(&installations, fs::Permissions::from_mode(0o755))
            .expect("broaden installations directory");
        let reader = LinuxStateRoot::open(root.path()).expect("open restored state root");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("broad parent must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        fs::set_permissions(&installations, fs::Permissions::from_mode(0o750))
            .expect("restore installations mode");

        let project_path = parent.join("project.json");
        fs::write(&project_path, b"state").expect("write state file");
        fs::set_permissions(&project_path, fs::Permissions::from_mode(0o644))
            .expect("broaden state-file mode");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("broad state file must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }

    #[test]
    fn hard_linked_state_file_is_rejected() {
        let root = TempTree::new("hard-linked-state");
        let parent = create_project_parent(root.path());
        let project_path = parent.join("project.json");
        fs::write(&project_path, b"state").expect("write state file");
        fs::set_permissions(&project_path, fs::Permissions::from_mode(0o600))
            .expect("set state-file mode");
        fs::hard_link(&project_path, parent.join("project-alias.json"))
            .expect("create state hard link");

        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("hard-linked state must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }

'''
if text.count(marker) != 1:
    raise SystemExit("expected symlinked-root test marker")
text = text.replace(marker, tests + marker, 1)

if "verify_directory(" in text or "verify_regular_file(" in text or "verify_private_mode(" in text:
    raise SystemExit("legacy verification helpers remain")
path.write_text(text, encoding="utf-8")
