use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::local_install_plan::{
    LocalInstallBuildPlan, LocalInstallGenerationIdentity, LocalInstallPlatform,
};
use crate::process::CommandSpec;

pub const LOCAL_INSTALL_BUILD_COMMAND_SCHEMA_VERSION: u8 = 2;
pub const LOCAL_INSTALL_BUILD_TIMEOUT: Duration = Duration::from_secs(20 * 60);
pub const MAX_LOCAL_INSTALL_BUILD_JOBS: u8 = 4;

const COMMAND_IDENTITY_DOMAIN: &[u8] = b"smolrunner-local-install-build-command-v2\0";
const CARGO_CONFIG_POLICY: &str = "isolated_cwd_and_cargo_home_config_free_v1";
const MACOS_SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const LINUX_SYSTEM_PATH: &str = "/usr/bin:/bin";
const FIXED_ARGUMENT_POLICY: [&str; 9] = [
    "build",
    "--manifest-path",
    "<private-source-manifest>",
    "--locked",
    "--offline",
    "--release",
    "--bin",
    "smolrunner",
    "--jobs",
];
const ENVIRONMENT_KEYS: [&str; 11] = [
    "CARGO_HOME",
    "CARGO_INCREMENTAL",
    "CARGO_NET_OFFLINE",
    "CARGO_TARGET_DIR",
    "CARGO_TERM_COLOR",
    "HOME",
    "LANG",
    "LC_ALL",
    "PATH",
    "RUSTC",
    "RUSTDOC",
];
const FIXED_PUBLIC_ENVIRONMENT: [&str; 5] = [
    "CARGO_INCREMENTAL=0",
    "CARGO_NET_OFFLINE=true",
    "CARGO_TERM_COLOR=never",
    "LANG=C",
    "LC_ALL=C",
];
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildCommandIdentity {
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildCommandPolicy {
    pub schema_version: u8,
    pub identity: LocalInstallBuildCommandIdentity,
    pub source_digest: Sha256Digest,
    pub target_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_predecessor: Option<LocalInstallGenerationIdentity>,
    pub platform: LocalInstallPlatform,
    pub jobs: u8,
    pub timeout_seconds: u64,
    pub cargo_config_policy: &'static str,
    pub system_path_policy: &'static str,
    pub fixed_argument_policy: [&'static str; 9],
    pub environment_keys: [&'static str; 11],
    pub fixed_public_environment: [&'static str; 5],
}

/// Private paths for one exact local self-build.
///
/// Callers choose one exact source root, one SmolRunner-owned build root, and three exact toolchain
/// executables. The command derives `work`, `home`, `cargo-home`, and `target` below the build root.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalInstallBuildCommandContext {
    source_root: PathBuf,
    build_root: PathBuf,
    cargo_program: PathBuf,
    rustc_program: PathBuf,
    rustdoc_program: PathBuf,
}

impl LocalInstallBuildCommandContext {
    /// Bind private lexical paths for one exact local self-build.
    ///
    /// # Errors
    ///
    /// Returns an error unless all paths are absolute normalized non-root UTF-8 paths, source and
    /// build roots are disjoint, and the toolchain executable paths are distinct.
    pub fn new(
        source_root: impl Into<PathBuf>,
        build_root: impl Into<PathBuf>,
        cargo_program: impl Into<PathBuf>,
        rustc_program: impl Into<PathBuf>,
        rustdoc_program: impl Into<PathBuf>,
    ) -> Result<Self, LocalInstallBuildCommandError> {
        let source_root = private_path(source_root.into())?;
        let build_root = private_path(build_root.into())?;
        let cargo_program = private_path(cargo_program.into())?;
        let rustc_program = private_path(rustc_program.into())?;
        let rustdoc_program = private_path(rustdoc_program.into())?;

        if overlaps(&source_root, &build_root) {
            return Err(error(
                LocalInstallBuildCommandErrorKind::UnsafeBuildDirectory,
                "unsafe_build_directory",
                "local self-build source and build roots must be disjoint",
            ));
        }
        if cargo_program == rustc_program
            || cargo_program == rustdoc_program
            || rustc_program == rustdoc_program
        {
            return Err(error(
                LocalInstallBuildCommandErrorKind::InvalidToolchainPaths,
                "invalid_toolchain_paths",
                "local self-build toolchain executable paths must be distinct",
            ));
        }

        Ok(Self {
            source_root,
            build_root,
            cargo_program,
            rustc_program,
            rustdoc_program,
        })
    }

    fn manifest_path(&self) -> PathBuf {
        self.source_root.join("Cargo.toml")
    }

    fn working_directory(&self) -> PathBuf {
        self.build_root.join("work")
    }

    fn isolated_home(&self) -> PathBuf {
        self.build_root.join("home")
    }

    fn cargo_home(&self) -> PathBuf {
        self.build_root.join("cargo-home")
    }

    fn target_directory(&self) -> PathBuf {
        self.build_root.join("target")
    }
}

impl fmt::Debug for LocalInstallBuildCommandContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalInstallBuildCommandContext")
            .field("source_root", &"<private exact source root>")
            .field("build_root", &"<private SmolRunner build root>")
            .field("cargo_program", &"<private reviewed toolchain executable>")
            .field("rustc_program", &"<private reviewed toolchain executable>")
            .field(
                "rustdoc_program",
                &"<private reviewed toolchain executable>",
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LocalInstallBuildCommand {
    policy: LocalInstallBuildCommandPolicy,
    spec: CommandSpec,
    working_directory: PathBuf,
    timeout: Duration,
}

impl LocalInstallBuildCommand {
    #[must_use]
    pub const fn policy(&self) -> &LocalInstallBuildCommandPolicy {
        &self.policy
    }

    #[must_use]
    pub const fn spec(&self) -> &CommandSpec {
        &self.spec
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl fmt::Debug for LocalInstallBuildCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalInstallBuildCommand")
            .field("policy", &self.policy)
            .field("program", &"<private reviewed Cargo executable>")
            .field("manifest", &"<private exact source manifest>")
            .field(
                "working_directory",
                &"<private isolated command working directory>",
            )
            .field("environment", &"<fixed reviewed private environment>")
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallBuildCommandErrorKind {
    InvalidJobs,
    UnsafePrivatePath,
    InvalidToolchainPaths,
    UnsafeBuildDirectory,
    IdentityEncodingFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildCommandError {
    pub kind: LocalInstallBuildCommandErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for LocalInstallBuildCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LocalInstallBuildCommandError {}

/// Build the sole reviewed offline Cargo command for one Z2-A install build plan.
///
/// Cargo runs from `<build-root>/work` and receives the exact source manifest through a redacted
/// `--manifest-path` argument. A later preflight proves the build-root lineage and isolated Cargo
/// home are config-free before this inert command may execute.
///
/// # Errors
///
/// Returns an error for jobs outside 1..=4 or canonical policy identity encoding failure.
pub fn plan_local_install_build_command(
    build: &LocalInstallBuildPlan,
    platform: LocalInstallPlatform,
    context: &LocalInstallBuildCommandContext,
    jobs: u8,
) -> Result<LocalInstallBuildCommand, LocalInstallBuildCommandError> {
    if !(1..=MAX_LOCAL_INSTALL_BUILD_JOBS).contains(&jobs) {
        return Err(error(
            LocalInstallBuildCommandErrorKind::InvalidJobs,
            "invalid_jobs",
            "local self-build jobs must be within the reviewed range",
        ));
    }

    let system_path = system_path(platform);
    let policy = policy(build, platform, jobs, system_path)?;
    let spec = CommandSpec::new(context.cargo_program.clone())
        .argument("build")
        .argument("--manifest-path")
        .secret_argument(private_utf8(&context.manifest_path()))
        .argument("--locked")
        .argument("--offline")
        .argument("--release")
        .argument("--bin")
        .argument("smolrunner")
        .argument("--jobs")
        .argument(jobs.to_string())
        .secret_environment("HOME", private_utf8(&context.isolated_home()))
        .secret_environment("CARGO_HOME", private_utf8(&context.cargo_home()))
        .secret_environment(
            "CARGO_TARGET_DIR",
            private_utf8(&context.target_directory()),
        )
        .secret_environment("RUSTC", private_utf8(&context.rustc_program))
        .secret_environment("RUSTDOC", private_utf8(&context.rustdoc_program))
        .environment("PATH", system_path)
        .environment("LANG", "C")
        .environment("LC_ALL", "C")
        .environment("CARGO_NET_OFFLINE", "true")
        .environment("CARGO_INCREMENTAL", "0")
        .environment("CARGO_TERM_COLOR", "never");

    Ok(LocalInstallBuildCommand {
        policy,
        spec,
        working_directory: context.working_directory(),
        timeout: LOCAL_INSTALL_BUILD_TIMEOUT,
    })
}

fn policy(
    build: &LocalInstallBuildPlan,
    platform: LocalInstallPlatform,
    jobs: u8,
    system_path: &'static str,
) -> Result<LocalInstallBuildCommandPolicy, LocalInstallBuildCommandError> {
    #[derive(Serialize)]
    struct IdentityDocument<'a> {
        schema_version: u8,
        source_digest: &'a Sha256Digest,
        target_generation: u64,
        expected_predecessor: &'a Option<LocalInstallGenerationIdentity>,
        platform: LocalInstallPlatform,
        jobs: u8,
        timeout_seconds: u64,
        cargo_config_policy: &'static str,
        system_path_policy: &'static str,
        fixed_argument_policy: [&'static str; 9],
        environment_keys: [&'static str; 11],
        fixed_public_environment: [&'static str; 5],
    }

    let timeout_seconds = LOCAL_INSTALL_BUILD_TIMEOUT.as_secs();
    let document = IdentityDocument {
        schema_version: LOCAL_INSTALL_BUILD_COMMAND_SCHEMA_VERSION,
        source_digest: &build.source.digest,
        target_generation: build.target_generation,
        expected_predecessor: &build.expected_predecessor,
        platform,
        jobs,
        timeout_seconds,
        cargo_config_policy: CARGO_CONFIG_POLICY,
        system_path_policy: system_path,
        fixed_argument_policy: FIXED_ARGUMENT_POLICY,
        environment_keys: ENVIRONMENT_KEYS,
        fixed_public_environment: FIXED_PUBLIC_ENVIRONMENT,
    };
    let bytes = serde_json::to_vec(&document).map_err(|_| identity_encoding_failed())?;
    let digest = domain_digest(&bytes)?;

    Ok(LocalInstallBuildCommandPolicy {
        schema_version: LOCAL_INSTALL_BUILD_COMMAND_SCHEMA_VERSION,
        identity: LocalInstallBuildCommandIdentity { digest },
        source_digest: build.source.digest.clone(),
        target_generation: build.target_generation,
        expected_predecessor: build.expected_predecessor.clone(),
        platform,
        jobs,
        timeout_seconds,
        cargo_config_policy: CARGO_CONFIG_POLICY,
        system_path_policy: system_path,
        fixed_argument_policy: FIXED_ARGUMENT_POLICY,
        environment_keys: ENVIRONMENT_KEYS,
        fixed_public_environment: FIXED_PUBLIC_ENVIRONMENT,
    })
}

const fn system_path(platform: LocalInstallPlatform) -> &'static str {
    match platform {
        LocalInstallPlatform::Macos => MACOS_SYSTEM_PATH,
        LocalInstallPlatform::Linux => LINUX_SYSTEM_PATH,
    }
}

fn private_path(path: PathBuf) -> Result<PathBuf, LocalInstallBuildCommandError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.to_str().is_none()
        || !path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(error(
            LocalInstallBuildCommandErrorKind::UnsafePrivatePath,
            "unsafe_private_path",
            "local self-build private path is unsafe or noncanonical",
        ));
    }
    Ok(path)
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn private_utf8(path: &Path) -> String {
    path.to_str()
        .expect("private paths are validated as UTF-8")
        .to_owned()
}

fn domain_digest(bytes: &[u8]) -> Result<Sha256Digest, LocalInstallBuildCommandError> {
    let mut hasher = Sha256::new();
    hasher.update(COMMAND_IDENTITY_DOMAIN);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| identity_encoding_failed())
}

const fn identity_encoding_failed() -> LocalInstallBuildCommandError {
    error(
        LocalInstallBuildCommandErrorKind::IdentityEncodingFailed,
        "identity_encoding_failed",
        "local self-build command identity could not be encoded",
    )
}

const fn error(
    kind: LocalInstallBuildCommandErrorKind,
    code: &'static str,
    problem: &'static str,
) -> LocalInstallBuildCommandError {
    LocalInstallBuildCommandError {
        kind,
        code,
        problem,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::local_install_plan::{
        LocalInstallBuildPlan, LocalInstallGenerationIdentity, LocalInstallSourceIdentity,
        LocalInstallToolchainIdentity,
    };
    use crate::process::CommandValue;

    use super::*;

    fn digest(ch: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", ch.to_string().repeat(64))).expect("digest")
    }

    fn source(ch: char) -> LocalInstallSourceIdentity {
        LocalInstallSourceIdentity::new(
            CommitId::parse(&ch.to_string().repeat(40)).expect("commit"),
            GitTreeId::parse(&ch.to_string().repeat(40)).expect("tree"),
            digest(ch),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
                .expect("toolchain"),
        )
        .expect("source")
    }

    fn build(ch: char) -> LocalInstallBuildPlan {
        LocalInstallBuildPlan {
            target_generation: 2,
            expected_predecessor: Some(LocalInstallGenerationIdentity {
                number: 1,
                digest: digest('f'),
            }),
            source: source(ch),
        }
    }

    fn context(prefix: &str) -> LocalInstallBuildCommandContext {
        LocalInstallBuildCommandContext::new(
            format!("/{prefix}/source"),
            format!("/{prefix}-build"),
            "/reviewed-toolchain/cargo",
            "/reviewed-toolchain/rustc",
            "/reviewed-toolchain/rustdoc",
        )
        .expect("context")
    }

    #[test]
    fn exact_argv_uses_redacted_manifest_and_fixed_build_root_children() {
        let context = context("private-a");
        let command =
            plan_local_install_build_command(&build('a'), LocalInstallPlatform::Macos, &context, 3)
                .expect("command");

        assert_eq!(
            command.spec().displayed_argv(),
            [
                context.cargo_program.to_string_lossy().to_string(),
                "build".to_owned(),
                "--manifest-path".to_owned(),
                "[REDACTED]".to_owned(),
                "--locked".to_owned(),
                "--offline".to_owned(),
                "--release".to_owned(),
                "--bin".to_owned(),
                "smolrunner".to_owned(),
                "--jobs".to_owned(),
                "3".to_owned(),
            ]
        );
        assert_eq!(command.working_directory(), context.working_directory());
        assert_ne!(command.working_directory(), context.source_root);
        assert!(matches!(
            command.spec().arguments[2],
            CommandValue::Secret(_)
        ));
        assert!(matches!(
            command.spec().environment.get("CARGO_HOME"),
            Some(CommandValue::Secret(_))
        ));
    }

    #[test]
    fn environment_is_closed_and_private_values_are_redacted() {
        let context = context("private-b");
        let command =
            plan_local_install_build_command(&build('a'), LocalInstallPlatform::Linux, &context, 2)
                .expect("command");
        let keys = command
            .spec()
            .environment
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(keys, ENVIRONMENT_KEYS);
        for key in ["HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "RUSTC", "RUSTDOC"] {
            assert!(matches!(
                command.spec().environment.get(key),
                Some(CommandValue::Secret(_))
            ));
        }
        for forbidden in [
            "RUSTFLAGS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "GIT_ASKPASS",
            "SSH_AUTH_SOCK",
        ] {
            assert!(!command.spec().environment.contains_key(forbidden));
        }
        let serialized = serde_json::to_string(command.spec()).expect("serialized spec");
        assert!(!serialized.contains("private-b"));
        assert!(serialized.contains("[REDACTED]"));
        assert_eq!(command.policy().system_path_policy, "/usr/bin:/bin");
    }

    #[test]
    fn public_policy_is_path_independent_and_binds_isolation_rules() {
        let build = build('a');
        let first = plan_local_install_build_command(
            &build,
            LocalInstallPlatform::Macos,
            &context("secret-one"),
            4,
        )
        .expect("first");
        let second = plan_local_install_build_command(
            &build,
            LocalInstallPlatform::Macos,
            &context("secret-two"),
            4,
        )
        .expect("second");
        assert_eq!(first.policy(), second.policy());
        assert_eq!(
            first.policy().cargo_config_policy,
            "isolated_cwd_and_cargo_home_config_free_v1"
        );
        let public = serde_json::to_string(first.policy()).expect("policy");
        assert!(!public.contains("secret-one"));
        assert!(!public.contains("secret-two"));
        assert!(!format!("{first:?}").contains("secret-one"));
    }

    #[test]
    fn platform_source_predecessor_and_jobs_change_policy_identity() {
        let context = context("private-c");
        let base =
            plan_local_install_build_command(&build('a'), LocalInstallPlatform::Macos, &context, 2)
                .expect("base");
        let source =
            plan_local_install_build_command(&build('b'), LocalInstallPlatform::Macos, &context, 2)
                .expect("source");
        let mut predecessor_build = build('a');
        predecessor_build.expected_predecessor = Some(LocalInstallGenerationIdentity {
            number: 1,
            digest: digest('e'),
        });
        let predecessor = plan_local_install_build_command(
            &predecessor_build,
            LocalInstallPlatform::Macos,
            &context,
            2,
        )
        .expect("predecessor");
        let jobs =
            plan_local_install_build_command(&build('a'), LocalInstallPlatform::Macos, &context, 3)
                .expect("jobs");
        let linux =
            plan_local_install_build_command(&build('a'), LocalInstallPlatform::Linux, &context, 2)
                .expect("linux");

        let identities = [base, source, predecessor, jobs, linux]
            .into_iter()
            .map(|command| command.policy().identity.digest.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(identities.len(), 5);
    }

    #[test]
    fn unsafe_overlap_jobs_and_tool_paths_fail_closed() {
        let build = build('a');
        let context = context("private-d");
        for jobs in [0, 5, u8::MAX] {
            assert_eq!(
                plan_local_install_build_command(
                    &build,
                    LocalInstallPlatform::Macos,
                    &context,
                    jobs,
                )
                .expect_err("jobs")
                .kind,
                LocalInstallBuildCommandErrorKind::InvalidJobs
            );
        }
        assert_eq!(
            LocalInstallBuildCommandContext::new(
                "/repo/source",
                "/repo/source/build",
                "/tools/cargo",
                "/tools/rustc",
                "/tools/rustdoc",
            )
            .expect_err("overlap")
            .kind,
            LocalInstallBuildCommandErrorKind::UnsafeBuildDirectory
        );
        assert_eq!(
            LocalInstallBuildCommandContext::new(
                "relative/source",
                "/build/root",
                "/tools/cargo",
                "/tools/rustc",
                "/tools/rustdoc",
            )
            .expect_err("relative")
            .kind,
            LocalInstallBuildCommandErrorKind::UnsafePrivatePath
        );
        assert_eq!(
            LocalInstallBuildCommandContext::new(
                "/repo/source",
                "/build/root",
                "/tools/cargo",
                "/tools/cargo",
                "/tools/rustdoc",
            )
            .expect_err("duplicate tool")
            .kind,
            LocalInstallBuildCommandErrorKind::InvalidToolchainPaths
        );
    }
}
