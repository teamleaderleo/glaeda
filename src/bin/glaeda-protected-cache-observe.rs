#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("protected hot-run cache observation requires Linux");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {

    use std::fs::{File, Metadata};
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Component, Path, PathBuf};
    use std::process::ExitCode;

    use clap::{Parser, ValueEnum};
    use glaeda::hot_run_cache_observation::{
        HotRunCacheObservationReport, build_hot_run_cache_observation_report,
        observe_owned_hot_run_cache, render_hot_run_cache_observation_human,
    };
    use rustix::fs::{self as rustix_fs, Mode, OFlags};
    use rustix::process::{geteuid, getuid};
    use rustix::thread::{CapabilitySet, CapabilitySets, capabilities};
    use serde::Serialize;
    use sha2::{Digest as _, Sha256};

    const REPORT_SCHEMA_VERSION: u8 = 1;
    const INSTALL_CONTRACT: &str = "root_owned_filecap_dac_read_search_v1";
    const REQUIRED_CAPABILITY: &str = "cap_dac_read_search_only";
    const INSTALLED_PROGRAM: &str = "/usr/libexec/glaeda/glaeda-protected-cache-observe";
    const INSTALL_PARENTS: [&str; 4] = ["/", "/usr", "/usr/libexec", "/usr/libexec/glaeda"];
    const MAX_INSTALLED_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;

    #[derive(Debug, Parser)]
    #[command(
        name = "glaeda-protected-cache-observe",
        about = "Observe one current-user-owned protected hot-run cache without mutation"
    )]
    struct Cli {
        /// Explicit canonical absolute hot-run cache root.
        #[arg(long)]
        root: PathBuf,

        /// Select human or JSON output.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
    }

    #[derive(Debug, Clone, Copy, ValueEnum)]
    enum OutputFormat {
        Human,
        Json,
    }

    #[derive(Debug, Serialize)]
    struct ProtectedCacheObservationReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        mutation_performed: bool,
        observer: ProtectedObserverReport,
        observation: HotRunCacheObservationReport,
    }

    impl ProtectedCacheObservationReport {
        fn render_human(&self) -> String {
            format!(
                "protected cache observer: authority={} install_contract={} capability={} uid_preserved={} executable={}\n{}",
                self.authority,
                self.observer.install_contract,
                self.observer.capability,
                self.observer.uid_preserved,
                self.observer.executable_sha256,
                render_hot_run_cache_observation_human(&self.observation),
            )
        }
    }

    #[derive(Debug, Serialize)]
    struct ProtectedObserverReport {
        install_contract: &'static str,
        capability: &'static str,
        uid_preserved: bool,
        executable_sha256: String,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ObserverError {
        code: &'static str,
        problem: &'static str,
    }

    impl ObserverError {
        const fn new(code: &'static str, problem: &'static str) -> Self {
            Self { code, problem }
        }
    }

    #[derive(Debug, Serialize)]
    struct ObserverErrorReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        error: ObserverErrorBody,
    }

    #[derive(Debug, Serialize)]
    struct ObserverErrorBody {
        code: &'static str,
        problem: &'static str,
    }

    impl From<ObserverError> for ObserverErrorReport {
        fn from(error: ObserverError) -> Self {
            Self {
                document_type: "glaeda-protected-hot-run-cache-observation-error",
                schema_version: REPORT_SCHEMA_VERSION,
                authority: "observation_only",
                error: ObserverErrorBody {
                    code: error.code,
                    problem: error.problem,
                },
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ExecutableSnapshot {
        device: u64,
        inode: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        link_count: u64,
        size: u64,
        ctime: i64,
        ctime_nsec: i64,
    }

    impl ExecutableSnapshot {
        fn from_metadata(metadata: &Metadata) -> Result<Self, ObserverError> {
            if !metadata.file_type().is_file() {
                return Err(installation_invalid());
            }
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                link_count: metadata.nlink(),
                size: metadata.len(),
                ctime: metadata.ctime(),
                ctime_nsec: metadata.ctime_nsec(),
            })
        }

        const fn has_installed_shape(self) -> bool {
            self.uid == 0
                && self.gid == 0
                && self.link_count == 1
                && self.mode & 0o7777 == 0o755
                && self.size > 0
                && self.size <= MAX_INSTALLED_PROGRAM_BYTES
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct InstallParentSnapshot {
        device: u64,
        inode: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        ctime: i64,
        ctime_nsec: i64,
    }

    impl InstallParentSnapshot {
        fn from_metadata(metadata: &Metadata) -> Result<Self, ObserverError> {
            if !metadata.file_type().is_dir()
                || metadata.uid() != 0
                || metadata.gid() != 0
                || metadata.mode() & 0o022 != 0
            {
                return Err(installation_invalid());
            }
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                ctime: metadata.ctime(),
                ctime_nsec: metadata.ctime_nsec(),
            })
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct RuntimeEvidence {
        real_uid: u32,
        effective_uid: u32,
        current_exe_exact: bool,
        installed: ExecutableSnapshot,
        executing: ExecutableSnapshot,
        capabilities: CapabilitySets,
    }

    fn validate_runtime_evidence(evidence: RuntimeEvidence) -> Result<(), ObserverError> {
        if evidence.real_uid == 0 || evidence.effective_uid != evidence.real_uid {
            return Err(uid_invalid());
        }
        if !evidence.current_exe_exact
            || !evidence.installed.has_installed_shape()
            || evidence.installed != evidence.executing
        {
            return Err(installation_invalid());
        }
        let required = CapabilitySet::DAC_READ_SEARCH;
        if evidence.capabilities.effective != required
            || evidence.capabilities.permitted != required
            || !evidence.capabilities.inheritable.is_empty()
        {
            return Err(capability_invalid());
        }
        Ok(())
    }

    struct RuntimeGuard {
        uid: u32,
        parents: Vec<InstallParentSnapshot>,
        installed: File,
        executing: File,
        snapshot: ExecutableSnapshot,
        executable_sha256: String,
    }

    impl RuntimeGuard {
        fn open() -> Result<Self, ObserverError> {
            let current_exe = std::env::current_exe().map_err(|_| installation_invalid())?;
            let parents = installation_parent_snapshots()?;
            let installed_link =
                std::fs::symlink_metadata(INSTALLED_PROGRAM).map_err(|_| installation_invalid())?;
            if installed_link.file_type().is_symlink() {
                return Err(installation_invalid());
            }
            let installed = open_nofollow(Path::new(INSTALLED_PROGRAM))?;
            let executing = File::open("/proc/self/exe").map_err(|_| installation_invalid())?;
            let installed_snapshot = ExecutableSnapshot::from_metadata(
                &installed.metadata().map_err(|_| installation_invalid())?,
            )?;
            let executing_snapshot = ExecutableSnapshot::from_metadata(
                &executing.metadata().map_err(|_| installation_invalid())?,
            )?;
            let observed_capabilities = capabilities(None).map_err(|_| capability_invalid())?;
            let uid = getuid().as_raw();
            validate_runtime_evidence(RuntimeEvidence {
                real_uid: uid,
                effective_uid: geteuid().as_raw(),
                current_exe_exact: current_exe == Path::new(INSTALLED_PROGRAM),
                installed: installed_snapshot,
                executing: executing_snapshot,
                capabilities: observed_capabilities,
            })?;
            let executable_sha256 = digest_executable(&executing, executing_snapshot.size)?;
            Ok(Self {
                uid,
                parents,
                installed,
                executing,
                snapshot: executing_snapshot,
                executable_sha256,
            })
        }

        fn revalidate(&self) -> Result<(), ObserverError> {
            if installation_parent_snapshots().map_err(|_| identity_changed())? != self.parents {
                return Err(identity_changed());
            }
            let installed = ExecutableSnapshot::from_metadata(
                &self.installed.metadata().map_err(|_| identity_changed())?,
            )
            .map_err(|_| identity_changed())?;
            let executing = ExecutableSnapshot::from_metadata(
                &self.executing.metadata().map_err(|_| identity_changed())?,
            )
            .map_err(|_| identity_changed())?;
            let path =
                open_nofollow(Path::new(INSTALLED_PROGRAM)).map_err(|_| identity_changed())?;
            let path_snapshot = ExecutableSnapshot::from_metadata(
                &path.metadata().map_err(|_| identity_changed())?,
            )
            .map_err(|_| identity_changed())?;
            let current_capabilities = capabilities(None).map_err(|_| capability_invalid())?;
            validate_runtime_evidence(RuntimeEvidence {
                real_uid: getuid().as_raw(),
                effective_uid: geteuid().as_raw(),
                current_exe_exact: true,
                installed: path_snapshot,
                executing,
                capabilities: current_capabilities,
            })
            .map_err(|error| {
                if error.code == capability_invalid().code || error.code == uid_invalid().code {
                    error
                } else {
                    identity_changed()
                }
            })?;
            if installed != self.snapshot
                || executing != self.snapshot
                || path_snapshot != self.snapshot
            {
                return Err(identity_changed());
            }
            Ok(())
        }
    }

    fn installation_parent_snapshots() -> Result<Vec<InstallParentSnapshot>, ObserverError> {
        INSTALL_PARENTS
            .iter()
            .map(|path| {
                let metadata =
                    std::fs::symlink_metadata(path).map_err(|_| installation_invalid())?;
                if metadata.file_type().is_symlink() {
                    return Err(installation_invalid());
                }
                InstallParentSnapshot::from_metadata(&metadata)
            })
            .collect()
    }

    fn open_nofollow(path: &Path) -> Result<File, ObserverError> {
        let fd = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| installation_invalid())?;
        Ok(File::from(fd))
    }

    fn digest_executable(file: &File, expected_size: u64) -> Result<String, ObserverError> {
        if expected_size == 0 || expected_size > MAX_INSTALLED_PROGRAM_BYTES {
            return Err(installation_invalid());
        }
        let mut reader = file.try_clone().map_err(|_| installation_invalid())?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut observed = 0_u64;
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|_| installation_invalid())?;
            if count == 0 {
                break;
            }
            observed = observed
                .checked_add(u64::try_from(count).map_err(|_| installation_invalid())?)
                .ok_or_else(installation_invalid)?;
            if observed > MAX_INSTALLED_PROGRAM_BYTES {
                return Err(installation_invalid());
            }
            digest.update(&buffer[..count]);
        }
        if observed != expected_size {
            return Err(identity_changed());
        }
        Ok(format!("sha256:{:x}", digest.finalize()))
    }

    fn canonical_root(root: &Path) -> Result<PathBuf, ObserverError> {
        if !root.is_absolute()
            || root.to_str().is_none()
            || root
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        {
            return Err(root_invalid());
        }
        let link = std::fs::symlink_metadata(root).map_err(|_| root_invalid())?;
        if link.file_type().is_symlink() || !link.is_dir() {
            return Err(root_invalid());
        }
        let canonical = std::fs::canonicalize(root).map_err(|_| root_invalid())?;
        if canonical != root {
            return Err(root_invalid());
        }
        Ok(canonical)
    }

    fn observe(root: &Path) -> Result<ProtectedCacheObservationReport, ObserverError> {
        let guard = RuntimeGuard::open()?;
        let root = canonical_root(root)?;
        let observation = observe_owned_hot_run_cache(&root, guard.uid).map_err(|error| {
            ObserverError::new(
                error.code(),
                "protected hot-run cache observation was unavailable",
            )
        })?;
        let observation = build_hot_run_cache_observation_report(observation).map_err(|error| {
            ObserverError::new(error.code(), "protected cache report could not be built")
        })?;
        guard.revalidate()?;
        Ok(ProtectedCacheObservationReport {
            document_type: "glaeda-protected-hot-run-cache-observation",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "observation_only",
            mutation_performed: false,
            observer: ProtectedObserverReport {
                install_contract: INSTALL_CONTRACT,
                capability: REQUIRED_CAPABILITY,
                uid_preserved: true,
                executable_sha256: guard.executable_sha256,
            },
            observation,
        })
    }

    fn installation_invalid() -> ObserverError {
        ObserverError::new(
            "protected_cache_observer_installation_invalid",
            "protected cache observer installation is unavailable or unsafe",
        )
    }

    fn identity_changed() -> ObserverError {
        ObserverError::new(
            "protected_cache_observer_identity_changed",
            "protected cache observer identity changed during observation",
        )
    }

    fn capability_invalid() -> ObserverError {
        ObserverError::new(
            "protected_cache_observer_capability_invalid",
            "protected cache observer requires exactly CAP_DAC_READ_SEARCH",
        )
    }

    fn uid_invalid() -> ObserverError {
        ObserverError::new(
            "protected_cache_observer_uid_invalid",
            "protected cache observer must retain one non-root invoking UID",
        )
    }

    fn root_invalid() -> ObserverError {
        ObserverError::new(
            "protected_cache_observer_root_invalid",
            "protected cache observer requires one canonical current-user-owned root",
        )
    }

    fn emit_error(format: OutputFormat, error: ObserverError) -> ExitCode {
        let report = ObserverErrorReport::from(error);
        match format {
            OutputFormat::Human => eprintln!(
                "protected cache observation unavailable: code={} problem={}",
                report.error.code, report.error.problem
            ),
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => eprintln!("{json}"),
                Err(_) => eprintln!("protected cache observation error could not be encoded"),
            },
        }
        ExitCode::from(2)
    }

    pub(super) fn main() -> ExitCode {
        let cli = Cli::parse();
        let report = match observe(&cli.root) {
            Ok(report) => report,
            Err(error) => return emit_error(cli.output, error),
        };
        match cli.output {
            OutputFormat::Human => print!("{}", report.render_human()),
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(json) => println!("{json}"),
                Err(_) => {
                    eprintln!("protected cache observation could not be encoded");
                    return ExitCode::from(2);
                }
            },
        }
        ExitCode::SUCCESS
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn executable() -> ExecutableSnapshot {
            ExecutableSnapshot {
                device: 10,
                inode: 20,
                mode: 0o100755,
                uid: 0,
                gid: 0,
                link_count: 1,
                size: 1024,
                ctime: 30,
                ctime_nsec: 40,
            }
        }

        fn capabilities() -> CapabilitySets {
            CapabilitySets {
                effective: CapabilitySet::DAC_READ_SEARCH,
                permitted: CapabilitySet::DAC_READ_SEARCH,
                inheritable: CapabilitySet::empty(),
            }
        }

        fn evidence() -> RuntimeEvidence {
            RuntimeEvidence {
                real_uid: 1000,
                effective_uid: 1000,
                current_exe_exact: true,
                installed: executable(),
                executing: executable(),
                capabilities: capabilities(),
            }
        }

        #[test]
        fn exact_installed_filecap_contract_is_accepted() {
            validate_runtime_evidence(evidence()).expect("accept exact observer runtime");
        }

        #[test]
        fn host_root_setuid_path_mismatch_and_mutable_installations_are_refused() {
            let mut root = evidence();
            root.real_uid = 0;
            root.effective_uid = 0;
            assert_eq!(
                validate_runtime_evidence(root)
                    .expect_err("refuse host root")
                    .code,
                uid_invalid().code
            );

            let mut setuid = evidence();
            setuid.effective_uid = 0;
            assert_eq!(
                validate_runtime_evidence(setuid)
                    .expect_err("refuse setuid execution")
                    .code,
                uid_invalid().code
            );

            let mut path_mismatch = evidence();
            path_mismatch.current_exe_exact = false;
            assert_eq!(
                validate_runtime_evidence(path_mismatch)
                    .expect_err("refuse executable path mismatch")
                    .code,
                installation_invalid().code
            );

            let mut writable = evidence();
            writable.installed.mode = 0o100775;
            assert_eq!(
                validate_runtime_evidence(writable)
                    .expect_err("refuse writable installation")
                    .code,
                installation_invalid().code
            );
        }

        #[test]
        fn missing_extra_or_inheritable_capabilities_are_refused() {
            let mut missing = evidence();
            missing.capabilities = CapabilitySets {
                effective: CapabilitySet::empty(),
                permitted: CapabilitySet::empty(),
                inheritable: CapabilitySet::empty(),
            };
            assert_eq!(
                validate_runtime_evidence(missing)
                    .expect_err("refuse missing capability")
                    .code,
                capability_invalid().code
            );

            let mut extra = evidence();
            extra.capabilities.effective |= CapabilitySet::DAC_OVERRIDE;
            extra.capabilities.permitted |= CapabilitySet::DAC_OVERRIDE;
            assert_eq!(
                validate_runtime_evidence(extra)
                    .expect_err("refuse extra capability")
                    .code,
                capability_invalid().code
            );

            let mut inheritable = evidence();
            inheritable.capabilities.inheritable = CapabilitySet::DAC_READ_SEARCH;
            assert_eq!(
                validate_runtime_evidence(inheritable)
                    .expect_err("refuse inheritable capability")
                    .code,
                capability_invalid().code
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
