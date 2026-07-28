from pathlib import Path

path = Path("src/renderprove_protected_mount.rs")
text = path.read_text(encoding="utf-8")

old_import = "use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};"
new_import = "use rustix::fs::{self, AtFlags, FileType, Gid, Mode, OFlags, Uid};"
if text.count(old_import) != 1:
    raise SystemExit(f"fs import anchor count: {text.count(old_import)}")
text = text.replace(old_import, new_import, 1)

old_flags = '''const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);'''
new_flags = '''const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const MUTABLE_DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);'''
if text.count(old_flags) != 1:
    raise SystemExit(f"flags anchor count: {text.count(old_flags)}")
text = text.replace(old_flags, new_flags, 1)

old_evidence_acquire = '''    let evidence_source =
        open_or_create_relative_directory(&project_source, request.evidence().directory())?;
    let evidence_identity = inspect_directory_identity(&evidence_source, "evidence")?;
    if evidence_identity == project_identity {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "evidence directory must be a strict child of the reviewed project",
        ));
    }

    let root_identity = inspect_directory_identity(&mount_root, "mount_root")?;
    let project_alias = create_alias_directory(&mount_root, "project", &root_identity)?;
    let evidence_alias = match create_alias_directory(&mount_root, "evidence", &root_identity) {'''
new_evidence_acquire = '''    let evidence_source =
        prepare_relative_directory(&project_source, request.evidence().directory())?;
    let evidence_identity = inspect_directory_identity(evidence_source.directory(), "evidence")?;
    if evidence_identity == project_identity {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "evidence directory must be a strict child of the reviewed project",
        ));
    }

    let project_alias = create_alias_directory(&mount_root, "project")?;
    let evidence_alias = match create_alias_directory(&mount_root, "evidence") {'''
if text.count(old_evidence_acquire) != 1:
    raise SystemExit(f"evidence acquisition anchor count: {text.count(old_evidence_acquire)}")
text = text.replace(old_evidence_acquire, new_evidence_acquire, 1)
text = text.replace(
    "        &evidence_source,\n        &mount_root,",
    "        evidence_source.directory(),\n        &mount_root,",
    1,
)
old_success = '''    Ok(RenderproveProtectedMountLease {
        receipt,
        mount_root,
        _project_source: project_source,
        _evidence_source: evidence_source,'''
new_success = '''    let evidence_source = evidence_source.commit();
    Ok(RenderproveProtectedMountLease {
        receipt,
        mount_root,
        _project_source: project_source,
        _evidence_source: evidence_source,'''
if text.count(old_success) != 1:
    raise SystemExit(f"lease success anchor count: {text.count(old_success)}")
text = text.replace(old_success, new_success, 1)

start = text.index("fn create_alias_directory(")
end = text.index("fn generate_alias_name(")
new_alias_block = r'''struct PendingAlias<'a> {
    mount_root: &'a OwnedFd,
    name: &'a OsStr,
    armed: bool,
}

impl<'a> PendingAlias<'a> {
    fn new(mount_root: &'a OwnedFd, name: &'a OsStr) -> Self {
        Self { mount_root, name, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingAlias<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.mount_root.as_fd(), self.name, AtFlags::REMOVEDIR);
        }
    }
}

fn create_alias_directory(
    mount_root: &OwnedFd,
    prefix: &str,
) -> Result<CreatedAlias, RenderproveProtectedMountError> {
    create_alias_directory_with_generator(mount_root, prefix, || generate_alias_name(prefix))
}

fn create_alias_directory_with_generator(
    mount_root: &OwnedFd,
    prefix: &str,
    mut generate: impl FnMut() -> Result<OsString, RenderproveProtectedMountError>,
) -> Result<CreatedAlias, RenderproveProtectedMountError> {
    for _ in 0..MAX_ALIAS_ATTEMPTS {
        let name = generate()?;
        if !valid_alias_name(prefix, &name) {
            return Err(RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                "alias",
                "generated Renderprove mount alias is invalid",
            ));
        }
        match fs::mkdirat(mount_root.as_fd(), &name, PRIVATE_DIRECTORY_MODE) {
            Ok(()) => {
                let mut pending = PendingAlias::new(mount_root, &name);
                let base = fs::openat(
                    mount_root.as_fd(),
                    &name,
                    MUTABLE_DIRECTORY_FLAGS,
                    Mode::empty(),
                )
                .map_err(|_| RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    "alias",
                    "created Renderprove mount alias could not be retained",
                ))?;
                fs::fchmod(&base, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "created Renderprove mount alias permissions could not be fixed",
                    )
                })?;
                inspect_directory_identity(&base, "alias")?;
                let root_stat = fs::fstat(mount_root).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "Renderprove mount root identity could not be inspected",
                    )
                })?;
                let base_stat = fs::fstat(&base).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "alias",
                        "Renderprove mount alias identity could not be inspected",
                    )
                })?;
                if base_stat.st_uid != root_stat.st_uid
                    || base_stat.st_gid != root_stat.st_gid
                    || base_stat.st_mode & 0o7777 != 0o700
                {
                    return Err(RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                        "alias",
                        "Renderprove mount alias ownership or mode is unsafe",
                    ));
                }
                pending.disarm();
                return Ok(CreatedAlias {
                    path: Path::new(RENDERPROVE_PROTECTED_MOUNT_ROOT).join(&name),
                    name,
                    base,
                });
            }
            Err(Errno::EXIST) => continue,
            Err(Errno::ACCESS | Errno::PERM) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::PermissionDenied,
                    "alias",
                    "Renderprove mount alias could not be created with current authority",
                ));
            }
            Err(_) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    "alias",
                    "Renderprove mount alias could not be created",
                ));
            }
        }
    }
    Err(RenderproveProtectedMountError::new(
        RenderproveProtectedMountErrorKind::AliasCollision,
        "alias",
        "Renderprove mount alias allocation was exhausted",
    ))
}

'''
text = text[:start] + new_alias_block + text[end:]

start = text.index("fn open_or_create_relative_directory(")
end = text.index("fn require_root_owned_mount_root(")
new_evidence_block = r'''struct CreatedEvidenceDirectory {
    parent: OwnedFd,
    name: OsString,
    directory: OwnedFd,
}

struct PreparedEvidenceDirectory {
    directory: Option<OwnedFd>,
    created: Vec<CreatedEvidenceDirectory>,
    committed: bool,
}

impl PreparedEvidenceDirectory {
    fn directory(&self) -> &OwnedFd {
        self.directory.as_ref().expect("prepared evidence directory is retained")
    }

    fn commit(mut self) -> OwnedFd {
        self.committed = true;
        self.directory.take().expect("prepared evidence directory is retained")
    }
}

impl Drop for PreparedEvidenceDirectory {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for created in self.created.iter().rev() {
            let _ = remove_created_evidence_directory(created);
        }
    }
}

struct PendingEvidenceDirectory<'a> {
    parent: &'a OwnedFd,
    name: &'a OsStr,
    armed: bool,
}

impl<'a> PendingEvidenceDirectory<'a> {
    fn new(parent: &'a OwnedFd, name: &'a OsStr) -> Self {
        Self { parent, name, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingEvidenceDirectory<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.parent.as_fd(), self.name, AtFlags::REMOVEDIR);
        }
    }
}

fn prepare_relative_directory(
    project: &OwnedFd,
    path: &Path,
) -> Result<PreparedEvidenceDirectory, RenderproveProtectedMountError> {
    let components = relative_components(path)?;
    let project_stat = fs::fstat(project).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "evidence",
            "reviewed project owner could not be inspected",
        )
    })?;
    if project_stat.st_uid == u32::MAX || project_stat.st_gid == u32::MAX {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "reviewed project owner is invalid for evidence creation",
        ));
    }
    let owner = (project_stat.st_uid, project_stat.st_gid);
    let current = io::dup(project).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "evidence",
            "reviewed project descriptor could not be retained for evidence traversal",
        )
    })?;
    let mut prepared = PreparedEvidenceDirectory {
        directory: Some(current),
        created: Vec::new(),
        committed: false,
    };

    for component in components {
        let current = prepared.directory.take().expect("prepared evidence directory is retained");
        let next = match fs::openat(current.as_fd(), component, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(directory) => {
                require_evidence_directory_policy(&directory, owner)?;
                directory
            }
            Err(Errno::NOENT) => {
                fs::mkdirat(current.as_fd(), component, PRIVATE_DIRECTORY_MODE).map_err(
                    |error| match error {
                        Errno::EXIST => RenderproveProtectedMountError::new(
                            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                            "evidence",
                            "evidence path changed during descriptor-relative creation",
                        ),
                        Errno::ACCESS | Errno::PERM => RenderproveProtectedMountError::new(
                            RenderproveProtectedMountErrorKind::PermissionDenied,
                            "evidence",
                            "evidence directory could not be created with current authority",
                        ),
                        _ => RenderproveProtectedMountError::new(
                            RenderproveProtectedMountErrorKind::Io,
                            "evidence",
                            "evidence directory could not be created",
                        ),
                    },
                )?;
                let mut pending = PendingEvidenceDirectory::new(&current, component);
                let directory = fs::openat(
                    current.as_fd(),
                    component,
                    MUTABLE_DIRECTORY_FLAGS,
                    Mode::empty(),
                )
                .map_err(|_| RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                    "evidence",
                    "created evidence directory could not be retained safely",
                ))?;
                let created_stat = fs::fstat(&directory).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "evidence",
                        "created evidence directory owner could not be inspected",
                    )
                })?;
                if (created_stat.st_uid, created_stat.st_gid) != owner {
                    fs::fchown(
                        &directory,
                        Some(Uid::from_raw(owner.0)),
                        Some(Gid::from_raw(owner.1)),
                    )
                    .map_err(|_| RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::PermissionDenied,
                        "evidence",
                        "created evidence directory ownership could not be bound to the reviewed project",
                    ))?;
                }
                fs::fchmod(&directory, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::PermissionDenied,
                        "evidence",
                        "created evidence directory permissions could not be fixed",
                    )
                })?;
                require_evidence_directory_policy(&directory, owner)?;
                let retained = io::dup(&directory).map_err(|_| {
                    RenderproveProtectedMountError::new(
                        RenderproveProtectedMountErrorKind::Io,
                        "evidence",
                        "created evidence directory identity could not be retained for rollback",
                    )
                })?;
                pending.disarm();
                drop(pending);
                prepared.created.push(CreatedEvidenceDirectory {
                    parent: current,
                    name: component.to_os_string(),
                    directory: retained,
                });
                directory
            }
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::UnsafeFilesystem,
                    "evidence",
                    "evidence path contains an alias or non-directory component",
                ));
            }
            Err(_) => {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::Io,
                    "evidence",
                    "evidence directory could not be opened safely",
                ));
            }
        };
        prepared.directory = Some(next);
    }
    Ok(prepared)
}

fn require_evidence_directory_policy(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), RenderproveProtectedMountError> {
    let stat = fs::fstat(directory).map_err(|_| {
        RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::Io,
            "evidence",
            "evidence directory identity could not be inspected",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o700 != 0o700
        || stat.st_mode & 0o022 != 0
    {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::UnsafeFilesystem,
            "evidence",
            "evidence directory ownership or mode is unsafe",
        ));
    }
    Ok(())
}

fn remove_created_evidence_directory(
    created: &CreatedEvidenceDirectory,
) -> Result<(), RenderproveProtectedMountError> {
    let reopened = match fs::openat(
        created.parent.as_fd(),
        &created.name,
        DIRECTORY_FLAGS,
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(()),
        Err(_) => {
            return Err(RenderproveProtectedMountError::new(
                RenderproveProtectedMountErrorKind::CleanupFailed,
                "evidence_rollback",
                "created evidence directory could not be verified for rollback",
            ));
        }
    };
    if inspect_directory_identity(&reopened, "evidence_rollback")?
        != inspect_directory_identity(&created.directory, "evidence_rollback")?
    {
        return Err(RenderproveProtectedMountError::new(
            RenderproveProtectedMountErrorKind::CleanupFailed,
            "evidence_rollback",
            "created evidence directory changed before rollback",
        ));
    }
    fs::unlinkat(
        created.parent.as_fd(),
        &created.name,
        AtFlags::REMOVEDIR,
    )
    .map_err(|_| RenderproveProtectedMountError::new(
        RenderproveProtectedMountErrorKind::CleanupFailed,
        "evidence_rollback",
        "created evidence directory could not be rolled back",
    ))
}

'''
text = text[:start] + new_evidence_block + text[end:]

old_alias_test_call = '''        let identity = inspect_directory_identity(&root_fd, "test").expect("identity");
        let collision = OsString::from("project-00000000000000000000000000000000");'''
new_alias_test_call = '''        let collision = OsString::from("project-00000000000000000000000000000000");'''
if text.count(old_alias_test_call) != 1:
    raise SystemExit(f"alias test identity anchor count: {text.count(old_alias_test_call)}")
text = text.replace(old_alias_test_call, new_alias_test_call, 1)
text = text.replace(
    '''        let alias = create_alias_directory_with_generator(
            &root_fd,
            "project",
            &identity,
            || Ok(candidates.next().expect("candidate")),
        )''',
    '''        let alias = create_alias_directory_with_generator(&root_fd, "project", || {
            Ok(candidates.next().expect("candidate"))
        })''',
    1,
)

insert_anchor = '''    #[test]
    fn errors_and_cleanup_receipts_are_bounded() {'''
insert_tests = '''    #[test]
    fn prepared_evidence_is_private_owned_and_rolls_back_until_commit() {
        let root = TempRoot::new("evidence-rollback");
        let project = root.open();
        let project_stat = fs::fstat(&project).expect("project stat");
        {
            let prepared = prepare_relative_directory(&project, Path::new("artifacts/renderprove"))
                .expect("prepare evidence");
            let evidence_stat = fs::fstat(prepared.directory()).expect("evidence stat");
            assert_eq!(evidence_stat.st_uid, project_stat.st_uid);
            assert_eq!(evidence_stat.st_gid, project_stat.st_gid);
            assert_eq!(evidence_stat.st_mode & 0o7777, 0o700);
        }
        assert!(!root.0.join("artifacts").exists());
    }

    #[test]
    fn pending_alias_removes_created_directory_on_failure() {
        let root = TempRoot::new("alias-rollback");
        let root_fd = root.open();
        let name = OsString::from("project-22222222222222222222222222222222");
        fs::mkdirat(root_fd.as_fd(), &name, PRIVATE_DIRECTORY_MODE).expect("create alias");
        {
            let _pending = PendingAlias::new(&root_fd, &name);
        }
        assert!(!root.0.join(&name).exists());
    }

    #[test]
    fn errors_and_cleanup_receipts_are_bounded() {'''
if text.count(insert_anchor) != 1:
    raise SystemExit(f"hardening test anchor count: {text.count(insert_anchor)}")
text = text.replace(insert_anchor, insert_tests, 1)

path.write_text(text, encoding="utf-8")
