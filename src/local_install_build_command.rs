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

pub const LOCAL_INSTALL_BUILD_COMMAND_SCHEMA_VERSION: u8 = 1;
pub const LOCAL_INSTALL_BUILD_TIMEOUT: Duration = Duration::from_secs(20 * 60);
pub const MAX_LOCAL_INSTALL_BUILD_JOBS: u8 = 4;

const COMMAND_IDENTITY_DOMAIN: &[u8] = b"smolrunner-local-install-build-command-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const MACOS_SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const LINUX_SYSTEM_PATH: &str = "/usr/bin:/bin";
const CARGO_CONFIG_POLICY: &str = "source_tree_only_no_ancestor_config_v1";
const FIXED_ARGUMENT_POLICY: [&str; 7] = [
    "build",
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
    pub fixed_argument_policy: [&'static str; 7],
    pub environment_keys: [&'static str; 11],
}

#[derive(Clone, PartialEq, Eq)]
pub struct LocalInstallBuildCommandContext {
    repository_root: PathBuf,
    cargo_program: PathBuf,
    rustc_program: PathBuf,
    rustdoc_program: PathBuf,
    isolated_home: PathBuf,
    cargo_home: PathBuf,
    target_directory: PathBuf,
}

impl LocalInstallBuildCommandContext {
    /// Bind private, lexically safe paths for one exact local self-build.
    ///
    /// This constructor performs no filesystem observation. A later execution adapter must prove
    /// type, owner, mode, alias, executable identity, and the command policy's Cargo-config
    /// precondition before using these paths as authority.
    ///
    /// # Errors
    ///
    /// Returns an error unless every path is absolute, normalized, non-root UTF-8. The three
    /// toolchain executables must be distinct, and writable build directories must be lexically
    /// disjoint from the repository checkout.
    pub fn new(
        repository_root: impl Into<PathBuf>,
        cargo_program: impl Into<PathBuf>,
        rustc_program: impl Into<PathBuf>,
        rustdoc_program: impl Into<PathBuf>,
        isolated_home: impl Into<PathBuf>,
        cargo_home: impl Into<PathBuf>,
        target_directory: impl Into<PathBuf>,
    ) -> Result<Self, LocalInstallBuildCommandError> {
        let repository_root = private_path(repository_root.into())?;
        let cargo_program = private_path(cargo_program.into())?;
        let rustc_program = private_path(rustc_program.into())?;
        let rustdoc_program = private_path(rustdoc_program.into())?;
        let isolated_home = private_path(isolated_home.into())?;
        let cargo_home = private_path(cargo_home.into())?;
        let target_directory = private_path(target_directory.into())?;

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

        for writable in [&isolated_home, &cargo_home, &target_directory] {
            if writable.starts_with(&repository_root) || repository_root.starts_with(writable) {
                return Err(error(
                    LocalInstallBuildCommandErrorKind::UnsafeBuildDirectory,
                    "unsafe_build_directory",
                    "local self-build writable directories must be disjoint from the source checkout",
                ));
            }
        }

        Ok(Self {
            repository_root,
            cargo_program,
            rustc_program,
            rustdoc_program,
            isolated_home,
            cargo_home,
            target_directory,
        })
    }
}

impl fmt::Debug for LocalInstallBuildCommandContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalInstallBuildCommandContext")
            .field("repository_root", &"<private absolute path>")
            .field("cargo_program", &"<private reviewed toolchain executable>")
            .field("rustc_program", &"<private reviewed toolchain executable>")
            .field("rustdoc_program", &"<private reviewed toolchain executable>")
            .field("isolated_home", &"<private SmolRunner build directory>")
            .field("cargo_home", &"<private SmolRunner build directory>")
            .field("target_directory", &"<private SmolRunner build directory>")
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
            .field("working_directory", &"<private source checkout>")
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
/// The returned `CommandSpec` is inert. The process layer clears ambient environment state when an
/// accepted execution adapter eventually runs it. No caller-controlled flags or environment values
/// are accepted by this API. Before launch, that adapter must additionally prove Cargo will load no
/// configuration from ancestors outside the exact source tree and no unreviewed config from the
/// isolated Cargo home; Cargo's hierarchical config discovery cannot be disabled by this pure
/// command object.
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

    let policy = policy(build, platform, jobs)?;
    let system_path = match platform {
        LocalInstallPlatform::Macos => MACOS_SYSTEM_PATH,
        LocalInstallPlatform::Linux => LINUX_SYSTEM_PATH,
    };

    let spec = CommandSpec::new(context.cargo_program.clone())
        .argument("build")
        .argument("--locked")
        .argument("--offline")
        .argument("--release")
        .argument("--bin")
        .argument("smolrunner")
        .argument("--jobs")
        .argument(jobs.to_string())
        .secret_environment("HOME", private_utf8(&context.isolated_home))
        .secret_environment("CARGO_HOME", private_utf8(&context.cargo_home))
        .secret_environment("CARGO_TARGET_DIR", private_utf8(&context.target_directory))
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
        working_directory: context.repository_root.clone(),
        timeout: LOCAL_INSTALL_BUILD_TIMEOUT,
    })
}

fn policy(
    build: &LocalInstallBuildPlan,
    platform: LocalInstallPlatform,
    jobs: u8,
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
        fixed_argument_policy: [&'static str; 7],
        environment_keys: [&'static str; 11],
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
        fixed_argument_policy: FIXED_ARGUMENT_POLICY,
        environment_keys: ENVIRONMENT_KEYS,
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
        fixed_argument_policy: FIXED_ARGUMENT_POLICY,
        environment_keys: ENVIRONMENT_KEYS,
    })
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
            format!("/{prefix}/toolchain/cargo"),
            format!("/{prefix}/toolchain/rustc"),
            format!("/{prefix}/toolchain/rustdoc"),
            format!("/{prefix}/state/home"),
            format!("/{prefix}/state/cargo-home"),
            format!("/{prefix}/state/target"),
        )
        .expect("context")
    }

    fn plain(value: &CommandValue) -> &str {
        match value {
            CommandValue::Plain(value) => value,
            CommandValue::Secret(_) => panic!("expected fixed public command value"),
        }
    }

    fn is_secret(value: &CommandValue) -> bool {
        matches!(value, CommandValue::Secret(_))
    }

    #[test]
    fn command_has_exact_fixed_argv_and_environment_keys() {
        let command = plan_local_install_build_command(
            &build('a'),
            LocalInstallPlatform::Macos,
            &context("private-a"),
            3,
        )
        .expect("command");

        let argv = command.spec().displayed_argv();
        assert_eq!(
            &argv[1..],
            [
                "build",
                "--locked",
                "--offline",
                "--release",
                "--bin",
                "smolrunner",
                "--jobs",
                "3",
            ]
        );
        let keys = command
            .spec()
            .environment
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(keys, ENVIRONMENT_KEYS);
        assert_eq!(
            command.policy().cargo_config_policy,
            "source_tree_only_no_ancestor_config_v1"
        );
        for forbidden in [
            "RUSTFLAGS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "CARGO_REGISTRIES_CRATES_IO_TOKEN",
            "GIT_ASKPASS",
            "SSH_AUTH_SOCK",
        ] {
            assert!(!command.spec().environment.contains_key(forbidden));
        }
    }

    #[test]
    fn private_context_values_are_redacted_inside_command_spec() {
        let context = context("private-b");
        let command = plan_local_install_build_command(
            &build('a'),
            LocalInstallPlatform::Linux,
            &context,
            2,
        )
        .expect("command");

        assert_eq!(command.spec().program, context.cargo_program);
        assert_eq!(command.working_directory(), context.repository_root.as_path());
        for key in ["HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "RUSTC", "RUSTDOC"] {
            assert!(is_secret(
                command.spec().environment.get(key).expect("private value")
            ));
        }
        assert_eq!(
            plain(command.spec().environment.get("PATH").expect("path")),
            LINUX_SYSTEM_PATH
        );
        assert_eq!(LINUX_SYSTEM_PATH, "/usr/bin:/bin");
        assert_eq!(
            plain(
                command
                    .spec()
                    .environment
                    .get("CARGO_NET_OFFLINE")
                    .expect("offline")
            ),
            "true"
        );
        let serialized = serde_json::to_string(command.spec()).expect("serialized command spec");
        assert!(!serialized.contains("private-b/state"));
        assert!(serialized.contains("[REDACTED]"));
        assert_eq!(command.timeout(), LOCAL_INSTALL_BUILD_TIMEOUT);
    }

    #[test]
    fn platform_changes_fixed_system_path() {
        let build = build('a');
        let context = context("private-c");
        let mac = plan_local_install_build_command(
            &build,
            LocalInstallPlatform::Macos,
            &context,
            1,
        )
        .expect("mac");
        let linux = plan_local_install_build_command(
            &build,
            LocalInstallPlatform::Linux,
            &context,
            1,
        )
        .expect("linux");
        assert_eq!(
            plain(mac.spec().environment.get("PATH").expect("path")),
            MACOS_SYSTEM_PATH
        );
        assert_eq!(
            plain(linux.spec().environment.get("PATH").expect("path")),
            LINUX_SYSTEM_PATH
        );
        assert_ne!(mac.policy().identity, linux.policy().identity);
    }

    #[test]
    fn policy_identity_is_deterministic_and_excludes_private_paths() {
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

        let public = serde_json::to_string(first.policy()).expect("public policy");
        for private in [
            "secret-one",
            "secret-two",
            "/Users/",
            "/home/",
            "CARGO_HOME=",
            "RUSTFLAGS",
            "credential",
            "proxy",
        ] {
            assert!(!public.contains(private), "leaked private marker: {private}");
        }
        let debug = format!("{first:?}");
        assert!(!debug.contains("secret-one"));
    }

    #[test]
    fn source_predecessor_jobs_and_platform_change_policy_identity() {
        let context = context("private-d");
        let base = plan_local_install_build_command(
            &build('a'),
            LocalInstallPlatform::Macos,
            &context,
            2,
        )
        .expect("base");
        let different_source = plan_local_install_build_command(
            &build('b'),
            LocalInstallPlatform::Macos,
            &context,
            2,
        )
        .expect("source");
        let mut predecessor_build = build('a');
        predecessor_build.expected_predecessor = Some(LocalInstallGenerationIdentity {
            number: 1,
            digest: digest('e'),
        });
        let different_predecessor = plan_local_install_build_command(
            &predecessor_build,
            LocalInstallPlatform::Macos,
            &context,
            2,
        )
        .expect("predecessor");
        let different_jobs = plan_local_install_build_command(
            &build('a'),
            LocalInstallPlatform::Macos,
            &context,
            3,
        )
        .expect("jobs");

        let identities = [
            base.policy().identity.digest.as_str(),
            different_source.policy().identity.digest.as_str(),
            different_predecessor.policy().identity.digest.as_str(),
            different_jobs.policy().identity.digest.as_str(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(identities.len(), 4);
    }

    #[test]
    fn invalid_jobs_and_unsafe_private_paths_fail_closed() {
        let build = build('a');
        let context = context("private-e");
        for jobs in [0, 5, u8::MAX] {
            assert_eq!(
                plan_local_install_build_command(
                    &build,
                    LocalInstallPlatform::Macos,
                    &context,
                    jobs
                )
                .expect_err("invalid jobs")
                .kind,
                LocalInstallBuildCommandErrorKind::InvalidJobs
            );
        }

        assert_eq!(
            LocalInstallBuildCommandContext::new(
                "relative/source",
                "/tools/cargo",
                "/tools/rustc",
                "/tools/rustdoc",
                "/state/home",
                "/state/cargo",
                "/state/target",
            )
            .expect_err("relative source")
            .kind,
            LocalInstallBuildCommandErrorKind::UnsafePrivatePath
        );
        assert_eq!(
            LocalInstallBuildCommandContext::new(
                "/repo/source",
                "/tools/cargo",
                "/tools/rustc",
                "/tools/rustdoc",
                "/repo/source/home",
                "/state/cargo",
                "/state/target",
            )
            .expect_err("writable source child")
            .kind,
            LocalInstallBuildCommandErrorKind::UnsafeBuildDirectory
        );
        assert_eq!(
            LocalInstallBuildCommandContext::new(
                "/repo/source",
                "/tools/cargo",
                "/tools/cargo",
                "/tools/rustdoc",
                "/state/home",
                "/state/cargo",
                "/state/target",
            )
            .expect_err("duplicate tool paths")
            .kind,
            LocalInstallBuildCommandErrorKind::InvalidToolchainPaths
        );
    }
}
