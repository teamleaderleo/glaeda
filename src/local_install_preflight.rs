use std::collections::BTreeSet;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::local_install_build_command::{
    LocalInstallBuildCommandPolicy, LOCAL_INSTALL_BUILD_COMMAND_SCHEMA_VERSION,
};
use crate::local_install_build_command::directory_preflight::{
    LocalInstallDirectoryBlockingCode, LocalInstallDirectoryPreflightReceipt,
    LocalInstallDirectoryRepairCode, LOCAL_INSTALL_DIRECTORY_PREFLIGHT_SCHEMA_VERSION,
};
use crate::local_install_build_command::toolchain_preflight::{
    LocalInstallToolchainPreflightReceipt, LOCAL_INSTALL_TOOLCHAIN_PREFLIGHT_SCHEMA_VERSION,
};
use crate::local_install_cargo_config_preflight::{
    LocalInstallCargoConfigBlockingCode, LocalInstallCargoConfigPreflightReceipt,
    LocalInstallCargoConfigRepairCode, LOCAL_INSTALL_CARGO_CONFIG_PREFLIGHT_SCHEMA_VERSION,
};
use crate::local_install_plan::{LocalInstallBuildPlan, LocalInstallGenerationIdentity};
use crate::local_install_source_preflight::{
    LocalInstallSourcePreflightReceipt, LOCAL_INSTALL_SOURCE_PREFLIGHT_SCHEMA_VERSION,
};

pub const LOCAL_INSTALL_PREFLIGHT_SCHEMA_VERSION: u8 = 1;
const ACCEPTED_CARGO_CONFIG_POLICY: &str = "isolated_cwd_and_cargo_home_config_free_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPreflightDisposition {
    Ready,
    Repairable,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPreflightBlockingCode {
    SourceSchemaUnsupported,
    ConfigSchemaUnsupported,
    ToolchainSchemaUnsupported,
    DirectorySchemaUnsupported,
    CommandSchemaUnsupported,
    SourceDigestMismatch,
    CommandSourceDigestMismatch,
    CommandGenerationMismatch,
    CommandPredecessorMismatch,
    ToolchainIdentityMismatch,
    CargoConfigPolicyMismatch,
    SourceBlocked,
    ConfigBlocked,
    ToolchainBlocked,
    DirectoryBlocked,
    InconsistentRepairEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPreflightRepairCode {
    CreateBuildRoot,
    CreateWork,
    CreateHome,
    CreateCargoHome,
    CreateTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallPreflightReceipt {
    schema_version: u8,
    source_digest: Sha256Digest,
    target_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_predecessor: Option<LocalInstallGenerationIdentity>,
    source_ready: bool,
    config_ready: bool,
    toolchain_ready: bool,
    directories_ready: bool,
    disposition: LocalInstallPreflightDisposition,
    blocking_codes: Vec<LocalInstallPreflightBlockingCode>,
    repair_codes: Vec<LocalInstallPreflightRepairCode>,
}

impl LocalInstallPreflightReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source_digest(&self) -> &Sha256Digest {
        &self.source_digest
    }

    #[must_use]
    pub const fn target_generation(&self) -> u64 {
        self.target_generation
    }

    #[must_use]
    pub const fn expected_predecessor(&self) -> Option<&LocalInstallGenerationIdentity> {
        self.expected_predecessor.as_ref()
    }

    #[must_use]
    pub const fn source_ready(&self) -> bool {
        self.source_ready
    }

    #[must_use]
    pub const fn config_ready(&self) -> bool {
        self.config_ready
    }

    #[must_use]
    pub const fn toolchain_ready(&self) -> bool {
        self.toolchain_ready
    }

    #[must_use]
    pub const fn directories_ready(&self) -> bool {
        self.directories_ready
    }

    #[must_use]
    pub const fn disposition(&self) -> LocalInstallPreflightDisposition {
        self.disposition
    }

    #[must_use]
    pub fn blocking_codes(&self) -> &[LocalInstallPreflightBlockingCode] {
        &self.blocking_codes
    }

    #[must_use]
    pub fn repair_codes(&self) -> &[LocalInstallPreflightRepairCode] {
        &self.repair_codes
    }
}

/// Purely compose sealed self-build evidence. This function performs no observation or mutation.
#[must_use]
pub fn compose_local_install_preflight(
    build: &LocalInstallBuildPlan,
    command: &LocalInstallBuildCommandPolicy,
    source: &LocalInstallSourcePreflightReceipt,
    config: &LocalInstallCargoConfigPreflightReceipt,
    toolchain: &LocalInstallToolchainPreflightReceipt,
    directories: &LocalInstallDirectoryPreflightReceipt,
) -> LocalInstallPreflightReceipt {
    let source_digest = build.source.digest().clone();
    let mut blockers = BTreeSet::new();
    let mut repairs = BTreeSet::new();

    if source.schema_version() != LOCAL_INSTALL_SOURCE_PREFLIGHT_SCHEMA_VERSION {
        blockers.insert(LocalInstallPreflightBlockingCode::SourceSchemaUnsupported);
    }
    if config.schema_version() != LOCAL_INSTALL_CARGO_CONFIG_PREFLIGHT_SCHEMA_VERSION {
        blockers.insert(LocalInstallPreflightBlockingCode::ConfigSchemaUnsupported);
    }
    if toolchain.schema_version() != LOCAL_INSTALL_TOOLCHAIN_PREFLIGHT_SCHEMA_VERSION {
        blockers.insert(LocalInstallPreflightBlockingCode::ToolchainSchemaUnsupported);
    }
    if directories.schema_version() != LOCAL_INSTALL_DIRECTORY_PREFLIGHT_SCHEMA_VERSION {
        blockers.insert(LocalInstallPreflightBlockingCode::DirectorySchemaUnsupported);
    }
    if command.schema_version != LOCAL_INSTALL_BUILD_COMMAND_SCHEMA_VERSION {
        blockers.insert(LocalInstallPreflightBlockingCode::CommandSchemaUnsupported);
    }

    if source.expected_source_digest() != &source_digest {
        blockers.insert(LocalInstallPreflightBlockingCode::SourceDigestMismatch);
    }
    if command.source_digest != source_digest {
        blockers.insert(LocalInstallPreflightBlockingCode::CommandSourceDigestMismatch);
    }
    if command.target_generation != build.target_generation {
        blockers.insert(LocalInstallPreflightBlockingCode::CommandGenerationMismatch);
    }
    if command.expected_predecessor != build.expected_predecessor {
        blockers.insert(LocalInstallPreflightBlockingCode::CommandPredecessorMismatch);
    }
    if toolchain.expected_toolchain() != build.source.toolchain() {
        blockers.insert(LocalInstallPreflightBlockingCode::ToolchainIdentityMismatch);
    }
    if command.cargo_config_policy != ACCEPTED_CARGO_CONFIG_POLICY {
        blockers.insert(LocalInstallPreflightBlockingCode::CargoConfigPolicyMismatch);
    }

    if !source.ready() {
        blockers.insert(LocalInstallPreflightBlockingCode::SourceBlocked);
    }
    if !toolchain.ready() {
        blockers.insert(LocalInstallPreflightBlockingCode::ToolchainBlocked);
    }

    let config_repair = classify_config(config);
    match config_repair {
        EvidenceDisposition::Ready => {}
        EvidenceDisposition::Repairable(config_repairs) => repairs.extend(config_repairs),
        EvidenceDisposition::Blocked => {
            blockers.insert(LocalInstallPreflightBlockingCode::ConfigBlocked);
        }
    }

    let directory_repair = classify_directories(directories, config_repair);
    match directory_repair {
        EvidenceDisposition::Ready => {}
        EvidenceDisposition::Repairable(directory_repairs) => repairs.extend(directory_repairs),
        EvidenceDisposition::Blocked => {
            blockers.insert(LocalInstallPreflightBlockingCode::DirectoryBlocked);
        }
    }

    if !repair_evidence_is_consistent(config, directories) {
        blockers.insert(LocalInstallPreflightBlockingCode::InconsistentRepairEvidence);
    }

    let source_ready = source.ready();
    let config_ready = config.ready();
    let toolchain_ready = toolchain.ready();
    let directories_ready = directories.ready();

    let (disposition, repair_codes) = if blockers.is_empty() {
        let repair_codes = repairs.into_iter().collect::<Vec<_>>();
        if repair_codes.is_empty() {
            (LocalInstallPreflightDisposition::Ready, repair_codes)
        } else {
            (LocalInstallPreflightDisposition::Repairable, repair_codes)
        }
    } else {
        (LocalInstallPreflightDisposition::Blocked, Vec::new())
    };

    LocalInstallPreflightReceipt {
        schema_version: LOCAL_INSTALL_PREFLIGHT_SCHEMA_VERSION,
        source_digest,
        target_generation: build.target_generation,
        expected_predecessor: build.expected_predecessor.clone(),
        source_ready,
        config_ready,
        toolchain_ready,
        directories_ready,
        disposition,
        blocking_codes: blockers.into_iter().collect(),
        repair_codes,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvidenceDisposition {
    Ready,
    Repairable(BTreeSet<LocalInstallPreflightRepairCode>),
    Blocked,
}

fn classify_config(config: &LocalInstallCargoConfigPreflightReceipt) -> EvidenceDisposition {
    if config.ready() {
        return EvidenceDisposition::Ready;
    }

    let mut repairs = BTreeSet::new();
    for repair in config.repair_codes() {
        match repair {
            LocalInstallCargoConfigRepairCode::CreateIsolatedBuildRoot => {
                repairs.insert(LocalInstallPreflightRepairCode::CreateBuildRoot);
            }
            LocalInstallCargoConfigRepairCode::CreateIsolatedCargoHome => {
                repairs.insert(LocalInstallPreflightRepairCode::CreateCargoHome);
            }
        }
    }
    if repairs.is_empty() {
        return EvidenceDisposition::Blocked;
    }

    let only_repairable_blockers = config.blocking_codes().iter().all(|blocker| {
        matches!(
            blocker,
            LocalInstallCargoConfigBlockingCode::BuildRootMissing
                | LocalInstallCargoConfigBlockingCode::CargoHomeMissing
        )
    });
    if only_repairable_blockers {
        EvidenceDisposition::Repairable(repairs)
    } else {
        EvidenceDisposition::Blocked
    }
}

fn classify_directories(
    directories: &LocalInstallDirectoryPreflightReceipt,
    config: EvidenceDisposition,
) -> EvidenceDisposition {
    if directories.ready() {
        return EvidenceDisposition::Ready;
    }
    if directories.repairable() {
        let repairs = directories
            .repair_codes()
            .iter()
            .map(|repair| match repair {
                LocalInstallDirectoryRepairCode::CreateWork => {
                    LocalInstallPreflightRepairCode::CreateWork
                }
                LocalInstallDirectoryRepairCode::CreateHome => {
                    LocalInstallPreflightRepairCode::CreateHome
                }
                LocalInstallDirectoryRepairCode::CreateCargoHome => {
                    LocalInstallPreflightRepairCode::CreateCargoHome
                }
                LocalInstallDirectoryRepairCode::CreateTarget => {
                    LocalInstallPreflightRepairCode::CreateTarget
                }
            })
            .collect::<BTreeSet<_>>();
        return EvidenceDisposition::Repairable(repairs);
    }

    let build_root_missing_only = directories.blocking_codes()
        == [LocalInstallDirectoryBlockingCode::BuildRootMissing];
    let config_repairs_build_root = matches!(
        config,
        EvidenceDisposition::Repairable(ref repairs)
            if repairs.contains(&LocalInstallPreflightRepairCode::CreateBuildRoot)
    );
    if build_root_missing_only && config_repairs_build_root {
        EvidenceDisposition::Repairable(BTreeSet::new())
    } else {
        EvidenceDisposition::Blocked
    }
}

fn repair_evidence_is_consistent(
    config: &LocalInstallCargoConfigPreflightReceipt,
    directories: &LocalInstallDirectoryPreflightReceipt,
) -> bool {
    let config_cargo_home_missing = config
        .repair_codes()
        .contains(&LocalInstallCargoConfigRepairCode::CreateIsolatedCargoHome);
    let directory_cargo_home_missing = directories
        .repair_codes()
        .contains(&LocalInstallDirectoryRepairCode::CreateCargoHome);

    config_cargo_home_missing == directory_cargo_home_missing
        || config
            .repair_codes()
            .contains(&LocalInstallCargoConfigRepairCode::CreateIsolatedBuildRoot)
}
