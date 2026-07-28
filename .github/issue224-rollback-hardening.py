from pathlib import Path
import re

path = Path('src/renderprove_protected_mount.rs')
text = path.read_text(encoding='utf-8')


def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{label} anchor count: {count}')
    text = text.replace(old, new, 1)


replace_once(
    '''    let evidence_alias = match create_alias_directory(&mount_root, "evidence") {
        Ok(alias) => alias,
        Err(error) => {
            let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
            return Err(error);
        }
    };''',
    '''    let evidence_alias = match create_alias_directory(&mount_root, "evidence") {
        Ok(alias) => alias,
        Err(error) => {
            return Err(prefer_cleanup_error(
                error,
                cleanup_created_aliases(&mount_root, &[&project_alias]),
            ));
        }
    };''',
    'evidence alias rollback',
)

replace_once(
    '''    ) {
        let _ = remove_alias_directory(&mount_root, &evidence_alias.name, &evidence_alias.base);
        let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
        return Err(error);
    }''',
    '''    ) {
        return Err(prefer_cleanup_error(
            error,
            cleanup_created_aliases(&mount_root, &[&evidence_alias, &project_alias]),
        ));
    }''',
    'attach failure alias rollback',
)

replace_once(
    '''        Err(error) => {
            let _ = detach_mount(&evidence_alias.path);
            let _ = detach_mount(&project_alias.path);
            let _ = remove_alias_directory(&mount_root, &evidence_alias.name, &evidence_alias.base);
            let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
            return Err(error);
        }
    };
    let evidence_alias_mount = match open_alias_mount(''',
    '''        Err(error) => {
            return Err(prefer_cleanup_error(
                error,
                cleanup_attached_aliases(&mount_root, &[&evidence_alias, &project_alias]),
            ));
        }
    };
    let evidence_alias_mount = match open_alias_mount(''',
    'project alias verification rollback',
)

replace_once(
    '''        Err(error) => {
            let _ = detach_mount(&evidence_alias.path);
            let _ = detach_mount(&project_alias.path);
            let _ = remove_alias_directory(&mount_root, &evidence_alias.name, &evidence_alias.base);
            let _ = remove_alias_directory(&mount_root, &project_alias.name, &project_alias.base);
            return Err(error);
        }
    };

    let receipt =''',
    '''        Err(error) => {
            return Err(prefer_cleanup_error(
                error,
                cleanup_attached_aliases(&mount_root, &[&evidence_alias, &project_alias]),
            ));
        }
    };

    let receipt =''',
    'evidence alias verification rollback',
)

helper_anchor = 'fn attach_mount_pair<B: MountOperations>('
helpers = '''fn record_cleanup_error(
    first_error: &mut Option<RenderproveProtectedMountError>,
    result: Result<(), RenderproveProtectedMountError>,
) {
    if let Err(error) = result {
        first_error.get_or_insert(error);
    }
}

fn cleanup_created_aliases(
    mount_root: &OwnedFd,
    aliases: &[&CreatedAlias],
) -> Result<(), RenderproveProtectedMountError> {
    let mut first_error = None;
    for alias in aliases {
        record_cleanup_error(
            &mut first_error,
            remove_alias_directory(mount_root, &alias.name, &alias.base),
        );
    }
    first_error.map_or(Ok(()), Err)
}

fn cleanup_attached_aliases(
    mount_root: &OwnedFd,
    aliases: &[&CreatedAlias],
) -> Result<(), RenderproveProtectedMountError> {
    let mut first_error = None;
    for alias in aliases {
        record_cleanup_error(&mut first_error, detach_mount(&alias.path));
    }
    for alias in aliases {
        record_cleanup_error(
            &mut first_error,
            remove_alias_directory(mount_root, &alias.name, &alias.base),
        );
    }
    first_error.map_or(Ok(()), Err)
}

fn prefer_cleanup_error(
    original: RenderproveProtectedMountError,
    cleanup: Result<(), RenderproveProtectedMountError>,
) -> RenderproveProtectedMountError {
    cleanup.err().unwrap_or(original)
}

fn attach_mount_pair<B: MountOperations>('''
if text.count(helper_anchor) != 1:
    raise SystemExit(f'cleanup helper insertion count: {text.count(helper_anchor)}')
text = text.replace(helper_anchor, helpers, 1)

pattern = re.compile(
    r'fn attach_mount_pair<B: MountOperations>\([\s\S]*?\n}\n\nfn map_mount_error',
)
replacement = '''fn attach_mount_pair<B: MountOperations>(
    backend: &mut B,
    project_source: &OwnedFd,
    evidence_source: &OwnedFd,
    mount_root: &OwnedFd,
    project_alias: &OsStr,
    evidence_alias: &OsStr,
    project_alias_path: &Path,
) -> Result<(), RenderproveProtectedMountError> {
    let project_mount = backend.clone_mount(project_source)?;
    backend.attach_mount(&project_mount, mount_root, project_alias)?;
    let evidence_mount = match backend.clone_mount(evidence_source) {
        Ok(mount) => mount,
        Err(error) => {
            return Err(prefer_cleanup_error(
                error,
                backend.detach_mount(project_alias_path),
            ));
        }
    };
    if let Err(error) = backend.attach_mount(&evidence_mount, mount_root, evidence_alias) {
        return Err(prefer_cleanup_error(
            error,
            backend.detach_mount(project_alias_path),
        ));
    }
    Ok(())
}

fn map_mount_error'''
text, count = pattern.subn(replacement, text, count=1)
if count != 1:
    raise SystemExit(f'attach_mount_pair replacement count: {count}')

replace_once(
    '''    struct FakeMountOperations {
        events: Vec<&'static str>,
        attach_calls: usize,
        fail_second_attach: bool,
    }''',
    '''    struct FakeMountOperations {
        events: Vec<&'static str>,
        attach_calls: usize,
        fail_second_attach: bool,
        fail_detach: bool,
    }''',
    'fake backend fields',
)

replace_once(
    '''        fn detach_mount(
            &mut self,
            _alias_path: &Path,
        ) -> Result<(), RenderproveProtectedMountError> {
            self.events.push("detach");
            Ok(())
        }''',
    '''        fn detach_mount(
            &mut self,
            _alias_path: &Path,
        ) -> Result<(), RenderproveProtectedMountError> {
            self.events.push("detach");
            if self.fail_detach {
                return Err(RenderproveProtectedMountError::new(
                    RenderproveProtectedMountErrorKind::CleanupFailed,
                    "cleanup",
                    "injected rollback failure",
                ));
            }
            Ok(())
        }''',
    'fake detach failure',
)

anchor = '''    #[test]
    fn attached_pair_has_no_rollback_after_complete_acquisition() {'''
test = '''    #[test]
    fn attached_pair_reports_cleanup_failure_when_project_rollback_fails() {
        let root = TempRoot::new("rollback-failure");
        let descriptor = root.open();
        let mut backend = FakeMountOperations {
            fail_second_attach: true,
            fail_detach: true,
            ..FakeMountOperations::default()
        };
        let error = attach_mount_pair(
            &mut backend,
            &descriptor,
            &descriptor,
            &descriptor,
            OsStr::new("project-00000000000000000000000000000000"),
            OsStr::new("evidence-00000000000000000000000000000000"),
            Path::new("/private/project-alias"),
        )
        .expect_err("rollback failure must fail closed");
        assert_eq!(error.kind(), RenderproveProtectedMountErrorKind::CleanupFailed);
        assert_eq!(error.stage(), "cleanup");
        assert_eq!(
            backend.events,
            ["clone", "attach", "clone", "attach", "detach"]
        );
    }

    #[test]
    fn attached_pair_has_no_rollback_after_complete_acquisition() {'''
if text.count(anchor) != 1:
    raise SystemExit(f'rollback failure test anchor count: {text.count(anchor)}')
text = text.replace(anchor, test, 1)

path.write_text(text, encoding='utf-8')
