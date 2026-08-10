use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

pub const LOCAL_INSTALL_CARGO_CONFIG_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const LOCAL_INSTALL_CARGO_CONFIG_OBSERVATION_RECEIPT_TYPE: &str =
    "smolrunner-local-install-cargo-config-observation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCargoConfigDisposition {
    Absent,
    Toml,
    Legacy,
    Both,
    Unsafe,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AncestorCargoConfigDisposition {
    Absent,
    Present,
    Unsafe,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoHomeConfigDisposition {
    Missing,
    Absent,
    Present,
    Unsafe,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallCargoConfigObservation {
    pub schema_version: u8,
    pub receipt_type: &'static str,
    pub source_config: SourceCargoConfigDisposition,
    pub ancestor_config: AncestorCargoConfigDisposition,
    pub cargo_home_config: CargoHomeConfigDisposition,
    pub ready: bool,
    pub blocking_codes: Vec<&'static str>,
    pub repair_codes: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallCargoConfigObservationErrorKind {
    UnsafeSourceRoot,
    UnsafeCargoHomePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallCargoConfigObservationError {
    pub kind: LocalInstallCargoConfigObservationErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for LocalInstallCargoConfigObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LocalInstallCargoConfigObservationError {}

/// Observe the exact Cargo configuration locations that may influence a local self-build.
///
/// The source root must already be the caller's exact canonical worktree root. This function never
/// follows a symlink to establish config absence and returns only path-private classifications.
///
/// # Errors
///
/// Returns a bounded error when the source root is not an existing canonical real directory or the
/// isolated Cargo-home path is lexically unsafe.
pub fn observe_local_install_cargo_config(
    source_root: &Path,
    cargo_home: &Path,
) -> Result<LocalInstallCargoConfigObservation, LocalInstallCargoConfigObservationError> {
    validate_source_root(source_root)?;
    validate_private_path(cargo_home).map_err(|_| unsafe_cargo_home_path())?;

    let source_config = observe_source_config(source_root);
    let ancestor_config = observe_ancestor_config(source_root);
    let cargo_home_config = observe_cargo_home_config(cargo_home);

    let mut blocking_codes = Vec::new();
    let mut repair_codes = Vec::new();

    match source_config {
        SourceCargoConfigDisposition::Absent
        | SourceCargoConfigDisposition::Toml
        | SourceCargoConfigDisposition::Legacy => {}
        SourceCargoConfigDisposition::Both => blocking_codes.push("source_cargo_config_conflict"),
        SourceCargoConfigDisposition::Unsafe => blocking_codes.push("source_cargo_config_unsafe"),
        SourceCargoConfigDisposition::Unknown => {
            blocking_codes.push("source_cargo_config_unknown");
        }
    }

    match ancestor_config {
        AncestorCargoConfigDisposition::Absent => {}
        AncestorCargoConfigDisposition::Present => {
            blocking_codes.push("ancestor_cargo_config_present");
        }
        AncestorCargoConfigDisposition::Unsafe => {
            blocking_codes.push("ancestor_cargo_config_unsafe");
        }
        AncestorCargoConfigDisposition::Unknown => {
            blocking_codes.push("ancestor_cargo_config_unknown");
        }
    }

    match cargo_home_config {
        CargoHomeConfigDisposition::Missing => repair_codes.push("create_isolated_cargo_home"),
        CargoHomeConfigDisposition::Absent => {}
        CargoHomeConfigDisposition::Present => {
            blocking_codes.push("isolated_cargo_home_config_present");
        }
        CargoHomeConfigDisposition::Unsafe => {
            blocking_codes.push("isolated_cargo_home_unsafe");
        }
        CargoHomeConfigDisposition::Unknown => {
            blocking_codes.push("isolated_cargo_home_unknown");
        }
    }

    Ok(LocalInstallCargoConfigObservation {
        schema_version: LOCAL_INSTALL_CARGO_CONFIG_OBSERVATION_SCHEMA_VERSION,
        receipt_type: LOCAL_INSTALL_CARGO_CONFIG_OBSERVATION_RECEIPT_TYPE,
        source_config,
        ancestor_config,
        cargo_home_config,
        ready: blocking_codes.is_empty(),
        blocking_codes,
        repair_codes,
    })
}

fn validate_source_root(
    source_root: &Path,
) -> Result<(), LocalInstallCargoConfigObservationError> {
    validate_private_path(source_root).map_err(|_| unsafe_source_root())?;
    let metadata = std::fs::symlink_metadata(source_root).map_err(|_| unsafe_source_root())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsafe_source_root());
    }
    let canonical = std::fs::canonicalize(source_root).map_err(|_| unsafe_source_root())?;
    if canonical != source_root {
        return Err(unsafe_source_root());
    }
    Ok(())
}

fn validate_private_path(path: &Path) -> Result<(), ()> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.to_str().is_none()
        || !path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(());
    }
    Ok(())
}

fn observe_source_config(source_root: &Path) -> SourceCargoConfigDisposition {
    match observe_dot_cargo(source_root) {
        PairObservation::Absent => SourceCargoConfigDisposition::Absent,
        PairObservation::Toml => SourceCargoConfigDisposition::Toml,
        PairObservation::Legacy => SourceCargoConfigDisposition::Legacy,
        PairObservation::Both => SourceCargoConfigDisposition::Both,
        PairObservation::Unsafe => SourceCargoConfigDisposition::Unsafe,
        PairObservation::Unknown => SourceCargoConfigDisposition::Unknown,
    }
}

fn observe_ancestor_config(source_root: &Path) -> AncestorCargoConfigDisposition {
    let Some(mut current) = source_root.parent() else {
        return AncestorCargoConfigDisposition::Unknown;
    };
    let mut saw_present = false;
    let mut saw_unknown = false;

    loop {
        match observe_dot_cargo(current) {
            PairObservation::Unsafe => return AncestorCargoConfigDisposition::Unsafe,
            PairObservation::Unknown => saw_unknown = true,
            PairObservation::Toml | PairObservation::Legacy | PairObservation::Both => {
                saw_present = true;
            }
            PairObservation::Absent => {}
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent;
    }

    if saw_unknown {
        AncestorCargoConfigDisposition::Unknown
    } else if saw_present {
        AncestorCargoConfigDisposition::Present
    } else {
        AncestorCargoConfigDisposition::Absent
    }
}

fn observe_cargo_home_config(cargo_home: &Path) -> CargoHomeConfigDisposition {
    match observe_path_components(cargo_home) {
        PathObservation::Missing => CargoHomeConfigDisposition::Missing,
        PathObservation::Unsafe => CargoHomeConfigDisposition::Unsafe,
        PathObservation::Unknown => CargoHomeConfigDisposition::Unknown,
        PathObservation::Ready => match observe_config_pair(cargo_home) {
            PairObservation::Absent => CargoHomeConfigDisposition::Absent,
            PairObservation::Toml | PairObservation::Legacy | PairObservation::Both => {
                CargoHomeConfigDisposition::Present
            }
            PairObservation::Unsafe => CargoHomeConfigDisposition::Unsafe,
            PairObservation::Unknown => CargoHomeConfigDisposition::Unknown,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathObservation {
    Missing,
    Ready,
    Unsafe,
    Unknown,
}

fn observe_path_components(path: &Path) -> PathObservation {
    let mut current = PathBuf::from("/");
    let components = path.components().skip(1).collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            return PathObservation::Unsafe;
        };
        current.push(part);
        let is_last = index + 1 == components.len();
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return PathObservation::Unsafe;
                }
                if !metadata.is_dir() {
                    return PathObservation::Unsafe;
                }
                if is_last {
                    return PathObservation::Ready;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return PathObservation::Missing;
            }
            Err(_) => return PathObservation::Unknown,
        }
    }
    PathObservation::Unsafe
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairObservation {
    Absent,
    Toml,
    Legacy,
    Both,
    Unsafe,
    Unknown,
}

fn observe_dot_cargo(parent: &Path) -> PairObservation {
    let cargo_dir = parent.join(".cargo");
    match std::fs::symlink_metadata(&cargo_dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return PairObservation::Unsafe;
            }
            observe_config_pair(&cargo_dir)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => PairObservation::Absent,
        Err(_) => PairObservation::Unknown,
    }
}

fn observe_config_pair(directory: &Path) -> PairObservation {
    let toml = observe_config_file(&directory.join("config.toml"));
    let legacy = observe_config_file(&directory.join("config"));

    if matches!(toml, ConfigFileObservation::Unsafe)
        || matches!(legacy, ConfigFileObservation::Unsafe)
    {
        return PairObservation::Unsafe;
    }
    if matches!(toml, ConfigFileObservation::Unknown)
        || matches!(legacy, ConfigFileObservation::Unknown)
    {
        return PairObservation::Unknown;
    }

    match (
        matches!(toml, ConfigFileObservation::Present),
        matches!(legacy, ConfigFileObservation::Present),
    ) {
        (false, false) => PairObservation::Absent,
        (true, false) => PairObservation::Toml,
        (false, true) => PairObservation::Legacy,
        (true, true) => PairObservation::Both,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFileObservation {
    Absent,
    Present,
    Unsafe,
    Unknown,
}

fn observe_config_file(path: &Path) -> ConfigFileObservation {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                ConfigFileObservation::Unsafe
            } else {
                ConfigFileObservation::Present
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => ConfigFileObservation::Absent,
        Err(_) => ConfigFileObservation::Unknown,
    }
}

const fn unsafe_source_root() -> LocalInstallCargoConfigObservationError {
    LocalInstallCargoConfigObservationError {
        kind: LocalInstallCargoConfigObservationErrorKind::UnsafeSourceRoot,
        code: "unsafe_source_root",
        problem: "local self-build source root is unsafe or aliased",
    }
}

const fn unsafe_cargo_home_path() -> LocalInstallCargoConfigObservationError {
    LocalInstallCargoConfigObservationError {
        kind: LocalInstallCargoConfigObservationErrorKind::UnsafeCargoHomePath,
        code: "unsafe_cargo_home_path",
        problem: "local self-build isolated Cargo-home path is unsafe",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-cargo-config-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("temp root");
            Self(fs::canonicalize(path).expect("canonical temp root"))
        }

        fn source(&self) -> PathBuf {
            let path = self.0.join("sandbox/source");
            fs::create_dir_all(&path).expect("source");
            fs::canonicalize(path).expect("canonical source")
        }

        fn cargo_home(&self) -> PathBuf {
            self.0.join("isolated/cargo-home")
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_config(path: &Path) {
        fs::write(path, b"[net]\noffline = true\n").expect("config");
    }

    #[test]
    fn absent_config_and_missing_isolated_home_are_ready() {
        let root = TempRoot::new("absent");
        let source = root.source();
        let receipt = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("observation");
        assert_eq!(receipt.source_config, SourceCargoConfigDisposition::Absent);
        assert_eq!(
            receipt.ancestor_config,
            AncestorCargoConfigDisposition::Absent
        );
        assert_eq!(
            receipt.cargo_home_config,
            CargoHomeConfigDisposition::Missing
        );
        assert!(receipt.ready);
        assert!(receipt.blocking_codes.is_empty());
        assert_eq!(receipt.repair_codes, ["create_isolated_cargo_home"]);
    }

    #[test]
    fn one_source_bound_config_is_allowed_but_both_are_blocked() {
        let root = TempRoot::new("source-config");
        let source = root.source();
        let cargo = source.join(".cargo");
        fs::create_dir(&cargo).expect("cargo dir");
        write_config(&cargo.join("config.toml"));
        let modern = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("modern");
        assert_eq!(modern.source_config, SourceCargoConfigDisposition::Toml);
        assert!(modern.ready);

        fs::rename(cargo.join("config.toml"), cargo.join("config")).expect("legacy");
        let legacy = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("legacy");
        assert_eq!(legacy.source_config, SourceCargoConfigDisposition::Legacy);
        assert!(legacy.ready);

        write_config(&cargo.join("config.toml"));
        let both = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("both");
        assert_eq!(both.source_config, SourceCargoConfigDisposition::Both);
        assert!(!both.ready);
        assert_eq!(both.blocking_codes, ["source_cargo_config_conflict"]);
    }

    #[test]
    fn source_cargo_aliases_are_unsafe() {
        let root = TempRoot::new("source-alias");
        let source = root.source();
        let external = root.0.join("external-cargo");
        fs::create_dir(&external).expect("external");
        write_config(&external.join("config.toml"));
        symlink(&external, source.join(".cargo")).expect("cargo symlink");
        let receipt = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("observation");
        assert_eq!(receipt.source_config, SourceCargoConfigDisposition::Unsafe);
        assert!(!receipt.ready);

        fs::remove_file(source.join(".cargo")).expect("remove alias");
        fs::create_dir(source.join(".cargo")).expect("cargo dir");
        symlink(
            external.join("config.toml"),
            source.join(".cargo/config.toml"),
        )
        .expect("config symlink");
        let receipt = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("observation");
        assert_eq!(receipt.source_config, SourceCargoConfigDisposition::Unsafe);
    }

    #[test]
    fn ancestor_config_or_unsafe_dot_cargo_blocks() {
        let root = TempRoot::new("ancestor");
        let source = root.source();
        let ancestor_cargo = root.0.join("sandbox/.cargo");
        fs::create_dir(&ancestor_cargo).expect("ancestor cargo");
        write_config(&ancestor_cargo.join("config.toml"));
        let present = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("present");
        assert_eq!(
            present.ancestor_config,
            AncestorCargoConfigDisposition::Present
        );
        assert!(!present.ready);
        assert!(present.blocking_codes.contains(&"ancestor_cargo_config_present"));

        fs::remove_dir_all(&ancestor_cargo).expect("remove ancestor");
        let external = root.0.join("external-ancestor");
        fs::create_dir(&external).expect("external");
        symlink(&external, &ancestor_cargo).expect("ancestor symlink");
        let unsafe_receipt = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("unsafe");
        assert_eq!(
            unsafe_receipt.ancestor_config,
            AncestorCargoConfigDisposition::Unsafe
        );
        assert!(!unsafe_receipt.ready);
    }

    #[test]
    fn isolated_cargo_home_config_and_aliases_block() {
        let root = TempRoot::new("cargo-home");
        let source = root.source();
        let cargo_home = root.cargo_home();
        fs::create_dir_all(&cargo_home).expect("cargo home");
        let empty = observe_local_install_cargo_config(&source, &cargo_home).expect("empty");
        assert_eq!(empty.cargo_home_config, CargoHomeConfigDisposition::Absent);
        assert!(empty.ready);

        write_config(&cargo_home.join("config"));
        let present = observe_local_install_cargo_config(&source, &cargo_home).expect("present");
        assert_eq!(present.cargo_home_config, CargoHomeConfigDisposition::Present);
        assert!(!present.ready);
        fs::remove_dir_all(root.0.join("isolated")).expect("remove home parent");

        let external = root.0.join("external-home");
        fs::create_dir(&external).expect("external home");
        fs::create_dir(root.0.join("isolated")).expect("isolated parent");
        symlink(&external, &cargo_home).expect("cargo home alias");
        let unsafe_receipt = observe_local_install_cargo_config(&source, &cargo_home)
            .expect("unsafe alias");
        assert_eq!(
            unsafe_receipt.cargo_home_config,
            CargoHomeConfigDisposition::Unsafe
        );
        assert!(!unsafe_receipt.ready);
    }

    #[test]
    fn unsafe_source_inputs_are_rejected_without_paths_in_errors() {
        let root = TempRoot::new("source-input");
        let source = root.source();
        let alias = root.0.join("source-alias");
        symlink(&source, &alias).expect("source alias");
        for candidate in [Path::new("relative/source"), alias.as_path()] {
            let error = observe_local_install_cargo_config(candidate, &root.cargo_home())
                .expect_err("unsafe source");
            assert_eq!(
                error.kind,
                LocalInstallCargoConfigObservationErrorKind::UnsafeSourceRoot
            );
            assert!(!error.to_string().contains(root.0.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn public_receipt_is_path_private_and_deterministic() {
        let root = TempRoot::new("privacy");
        let source = root.source();
        let first = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("first");
        let second = observe_local_install_cargo_config(&source, &root.cargo_home())
            .expect("second");
        assert_eq!(first, second);
        let json = serde_json::to_string(&first).expect("json");
        assert!(!json.contains(root.0.to_string_lossy().as_ref()));
        assert!(!json.contains("sandbox/source"));
        assert!(!json.contains("isolated/cargo-home"));
    }
}
