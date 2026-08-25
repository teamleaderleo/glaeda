//! Pure ownership vocabulary for resident project sandboxes.
//!
//! This module deliberately stops at the catalog/model boundary.  It allocates a logical
//! resident generation and derives its Lima locator, but it does not open a store, observe a
//! host, invoke Lima, or create a current runtime capability.  The identity records here are
//! declarations and durable-history inputs only. `ProjectDiskLimaSourceIdentity` is the shared
//! configured-namespace equality identity; it is not the still-pending #699 durable physical
//! source discriminator and cannot establish a surviving host binding.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_host_observation::{LimaStandaloneDiskName, ProjectDiskLimaSourceIdentity};
use crate::project_disk_lease::{
    ProjectDiskGeneration, ProjectDiskId, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const RESIDENT_SANDBOX_CATALOG_SCHEMA_VERSION: u8 = 1;
pub const MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_RESIDENT_SANDBOX_ENTRIES: usize = 512;
pub const MAX_RESIDENT_SANDBOX_ACCEPTANCE_REQUESTS: usize = MAX_RESIDENT_SANDBOX_ENTRIES;
pub const MAX_RESIDENT_SANDBOX_LOCATOR_CLAIMS: usize = MAX_RESIDENT_SANDBOX_ENTRIES;
pub const MAX_RESIDENT_SANDBOX_LINEAGES: usize = 512;
pub const MAX_RESIDENT_SANDBOX_GENERATION: u64 = 1_000_000_000_000;
pub const MAX_RESIDENT_SANDBOX_REVISION: u64 = 1_000_000_000_000;
pub const MAX_RESIDENT_SANDBOX_LOCATOR_BYTES: usize = 64;

const MAX_IDENTIFIER_BYTES: usize = 96;
const MAX_POLICY_GENERATION: u64 = 1_000_000_000_000;
const MAX_CPU_MILLIS: u32 = 1_024_000;
const MAX_MEMORY_BYTES: u64 = 1 << 50;
const MAX_ROOT_DISK_BYTES: u64 = 1 << 50;
const LOCATOR_POLICY_DOMAIN: &[u8] = b"smolrunner-resident-lima-locator-v1";
const CONFIG_DIGEST_DOMAIN: &[u8] = b"smolrunner-resident-config-v1";
const OPERATION_POLICY_DOMAIN: &[u8] = b"smolrunner-resident-operation-policy-v1";

/// A bounded idempotency key allocated by the caller, not a generation or locator choice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResidentSandboxAcceptanceRequestId(String);

impl ResidentSandboxAcceptanceRequestId {
    pub fn parse(value: &str) -> Result<Self, ResidentSandboxCatalogError> {
        validate_identifier(value, "acceptance request id")?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! policy_generation_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ResidentSandboxCatalogError> {
                if !(1..=MAX_POLICY_GENERATION).contains(&value) {
                    return Err(error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident policy generation is outside the bounded positive range",
                    ));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

policy_generation_type!(ResidentPreparedTemplateGeneration);
policy_generation_type!(ResidentSandboxConfigGeneration);
policy_generation_type!(ResidentLimaLayoutGeneration);
policy_generation_type!(ResidentResourceGeneration);
policy_generation_type!(ResidentNetworkPolicyGeneration);
policy_generation_type!(ResidentCredentialPolicyGeneration);
policy_generation_type!(ResidentGuestControlPolicyGeneration);
policy_generation_type!(ResidentGuestPrivilegePolicyGeneration);
policy_generation_type!(ResidentProjectIntegrationPolicyGeneration);
policy_generation_type!(ResidentLocatorPolicyGeneration);

/// The only V1 backend/configuration combination admitted by this owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentSandboxBackend {
    Vz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentSandboxArchitecture {
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentSandboxTrustClass {
    UltraTrustedProject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentTaskPasswordPolicy {
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentControllerRootEscalation {
    ReviewedNonInteractiveSudo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentTaskRootEscalation {
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentTaskControlMutation {
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentProjectDiskAccess {
    Writable,
}

/// Exact guest account/privilege declaration bound by a resident config generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentGuestPrivilegePolicy {
    generation: ResidentGuestPrivilegePolicyGeneration,
    controller_account: String,
    controller_is_sole_normal_login: bool,
    controller_root_escalation: ResidentControllerRootEscalation,
    task_account: String,
    task_is_distinct_from_controller_and_root: bool,
    task_password_policy: ResidentTaskPasswordPolicy,
    task_supplementary_groups: Vec<String>,
    task_root_escalation: ResidentTaskRootEscalation,
    task_control_mutation: ResidentTaskControlMutation,
}

impl ResidentGuestPrivilegePolicy {
    /// Construct the reviewed V1 account split.  The account names and denial policies are fixed
    /// here so a caller cannot silently weaken the guest privilege boundary.
    pub fn reviewed(generation: ResidentGuestPrivilegePolicyGeneration) -> Self {
        Self {
            generation,
            controller_account: "smolrunner-admin".to_owned(),
            controller_is_sole_normal_login: true,
            controller_root_escalation:
                ResidentControllerRootEscalation::ReviewedNonInteractiveSudo,
            task_account: "smolrunner-runner".to_owned(),
            task_is_distinct_from_controller_and_root: true,
            task_password_policy: ResidentTaskPasswordPolicy::Locked,
            task_supplementary_groups: Vec::new(),
            task_root_escalation: ResidentTaskRootEscalation::Denied,
            task_control_mutation: ResidentTaskControlMutation::Denied,
        }
    }

    #[must_use]
    pub const fn generation(&self) -> ResidentGuestPrivilegePolicyGeneration {
        self.generation
    }

    #[must_use]
    pub fn controller_account(&self) -> &str {
        &self.controller_account
    }

    #[must_use]
    pub fn task_account(&self) -> &str {
        &self.task_account
    }

    #[must_use]
    pub fn task_supplementary_groups(&self) -> &[String] {
        &self.task_supplementary_groups
    }
}

/// Resource declaration retained as part of the immutable resident configuration identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResidentResourceDeclaration {
    generation: ResidentResourceGeneration,
    cpu_millis: u32,
    memory_bytes: u64,
    root_disk_bytes: u64,
}

impl ResidentResourceDeclaration {
    pub fn new(
        generation: ResidentResourceGeneration,
        cpu_millis: u32,
        memory_bytes: u64,
        root_disk_bytes: u64,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        if !(1..=MAX_CPU_MILLIS).contains(&cpu_millis)
            || !(1..=MAX_MEMORY_BYTES).contains(&memory_bytes)
            || !(1..=MAX_ROOT_DISK_BYTES).contains(&root_disk_bytes)
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident resource declaration is outside the bounded positive range",
            ));
        }
        Ok(Self {
            generation,
            cpu_millis,
            memory_bytes,
            root_disk_bytes,
        })
    }

    #[must_use]
    pub const fn generation(self) -> ResidentResourceGeneration {
        self.generation
    }

    #[must_use]
    pub const fn cpu_millis(self) -> u32 {
        self.cpu_millis
    }

    #[must_use]
    pub const fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }

    #[must_use]
    pub const fn root_disk_bytes(self) -> u64 {
        self.root_disk_bytes
    }
}

/// Optional immutable disk-bearing config declaration. The locator and expected P3 create
/// provenance are data-only equality claims and grant no disk lock, observation, attachment, or
/// mutation authority; the configured source identity is repeated to prevent cross-namespace
/// pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentProjectDiskConfigBinding {
    source_identity: ProjectDiskLimaSourceIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    locator: LimaStandaloneDiskName,
    create_provenance_identity: Sha256Digest,
    access: ResidentProjectDiskAccess,
}

impl ResidentProjectDiskConfigBinding {
    pub fn new(
        source_identity: ProjectDiskLimaSourceIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        locator: LimaStandaloneDiskName,
        create_provenance_identity: Sha256Digest,
    ) -> Self {
        Self {
            source_identity,
            disk_id,
            disk_generation,
            locator,
            create_provenance_identity,
            access: ResidentProjectDiskAccess::Writable,
        }
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn disk_id(&self) -> &ProjectDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn disk_generation(&self) -> ProjectDiskGeneration {
        self.disk_generation
    }

    #[must_use]
    pub const fn locator(&self) -> &LimaStandaloneDiskName {
        &self.locator
    }

    /// Opaque expected P3 create-provenance declaration. Equality is data only; it grants no
    /// project-disk lock, attachment, observation, or mutation authority.
    #[must_use]
    pub const fn create_provenance_identity(&self) -> &Sha256Digest {
        &self.create_provenance_identity
    }

    #[must_use]
    pub const fn access(&self) -> ResidentProjectDiskAccess {
        self.access
    }
}

/// Complete reviewed resident VM configuration declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxConfig {
    prepared_template_generation: ResidentPreparedTemplateGeneration,
    sandbox_config_generation: ResidentSandboxConfigGeneration,
    trust_class: ResidentSandboxTrustClass,
    backend: ResidentSandboxBackend,
    architecture: ResidentSandboxArchitecture,
    lima_layout_policy_generation: ResidentLimaLayoutGeneration,
    resources: ResidentResourceDeclaration,
    network_policy_generation: ResidentNetworkPolicyGeneration,
    credential_policy_generation: ResidentCredentialPolicyGeneration,
    guest_control_policy_generation: ResidentGuestControlPolicyGeneration,
    guest_privilege_policy: ResidentGuestPrivilegePolicy,
    project_integration_policy_generation: ResidentProjectIntegrationPolicyGeneration,
    auto_format: bool,
    automount: bool,
    project_disk: Option<ResidentProjectDiskConfigBinding>,
}

impl ResidentSandboxConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn reviewed(
        prepared_template_generation: ResidentPreparedTemplateGeneration,
        sandbox_config_generation: ResidentSandboxConfigGeneration,
        lima_layout_policy_generation: ResidentLimaLayoutGeneration,
        resources: ResidentResourceDeclaration,
        network_policy_generation: ResidentNetworkPolicyGeneration,
        credential_policy_generation: ResidentCredentialPolicyGeneration,
        guest_control_policy_generation: ResidentGuestControlPolicyGeneration,
        guest_privilege_policy: ResidentGuestPrivilegePolicy,
        project_integration_policy_generation: ResidentProjectIntegrationPolicyGeneration,
        project_disk: Option<ResidentProjectDiskConfigBinding>,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        if guest_privilege_policy.controller_account() != "smolrunner-admin"
            || guest_privilege_policy.task_account() != "smolrunner-runner"
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "guest privilege policy is not the reviewed V1 policy",
            ));
        }
        Ok(Self {
            prepared_template_generation,
            sandbox_config_generation,
            trust_class: ResidentSandboxTrustClass::UltraTrustedProject,
            backend: ResidentSandboxBackend::Vz,
            architecture: ResidentSandboxArchitecture::Aarch64,
            lima_layout_policy_generation,
            resources,
            network_policy_generation,
            credential_policy_generation,
            guest_control_policy_generation,
            guest_privilege_policy,
            project_integration_policy_generation,
            auto_format: false,
            automount: false,
            project_disk,
        })
    }

    #[must_use]
    pub const fn prepared_template_generation(&self) -> ResidentPreparedTemplateGeneration {
        self.prepared_template_generation
    }

    #[must_use]
    pub const fn sandbox_config_generation(&self) -> ResidentSandboxConfigGeneration {
        self.sandbox_config_generation
    }

    #[must_use]
    pub const fn trust_class(&self) -> ResidentSandboxTrustClass {
        self.trust_class
    }

    #[must_use]
    pub const fn backend(&self) -> ResidentSandboxBackend {
        self.backend
    }

    #[must_use]
    pub const fn architecture(&self) -> ResidentSandboxArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn lima_layout_policy_generation(&self) -> ResidentLimaLayoutGeneration {
        self.lima_layout_policy_generation
    }

    #[must_use]
    pub const fn resources(&self) -> ResidentResourceDeclaration {
        self.resources
    }

    #[must_use]
    pub const fn network_policy_generation(&self) -> ResidentNetworkPolicyGeneration {
        self.network_policy_generation
    }

    #[must_use]
    pub const fn credential_policy_generation(&self) -> ResidentCredentialPolicyGeneration {
        self.credential_policy_generation
    }

    #[must_use]
    pub const fn guest_control_policy_generation(&self) -> ResidentGuestControlPolicyGeneration {
        self.guest_control_policy_generation
    }

    #[must_use]
    pub const fn guest_privilege_policy(&self) -> &ResidentGuestPrivilegePolicy {
        &self.guest_privilege_policy
    }

    #[must_use]
    pub const fn project_integration_policy_generation(
        &self,
    ) -> ResidentProjectIntegrationPolicyGeneration {
        self.project_integration_policy_generation
    }

    #[must_use]
    pub const fn auto_format(&self) -> bool {
        self.auto_format
    }

    #[must_use]
    pub const fn automount(&self) -> bool {
        self.automount
    }

    #[must_use]
    pub fn project_disk(&self) -> Option<&ResidentProjectDiskConfigBinding> {
        self.project_disk.as_ref()
    }

    /// Compute the canonical digest of every config declaration, excluding the digest itself.
    pub fn digest(&self) -> Result<Sha256Digest, ResidentSandboxCatalogError> {
        let bytes = serde_json::to_vec(&ResidentSandboxConfigWire::from(self)).map_err(|_| {
            error(
                ResidentSandboxCatalogErrorKind::CorruptState,
                "resident config could not be canonically encoded",
            )
        })?;
        let mut hasher = Sha256::new();
        hasher.update(CONFIG_DIGEST_DOMAIN);
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        let digest = format!("sha256:{:x}", hasher.finalize());
        Sha256Digest::parse(&digest).map_err(|_| {
            error(
                ResidentSandboxCatalogErrorKind::CorruptState,
                "resident config digest could not be constructed",
            )
        })
    }
}

/// Lima-safe locator controlled by the catalog.  There is intentionally no public parser or
/// constructor; callers receive this only from an accepted catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResidentSandboxLocator(String);

impl ResidentSandboxLocator {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
struct ResidentSandboxLineage {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResidentSandboxCatalogRevision(u64);

impl ResidentSandboxCatalogRevision {
    pub fn new(value: u64) -> Result<Self, ResidentSandboxCatalogError> {
        if !(1..=MAX_RESIDENT_SANDBOX_REVISION).contains(&value) {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident catalog revision is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Semantic revision of one exact resident generation, distinct from the catalog CAS revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResidentSandboxRecordRevision(u64);

impl ResidentSandboxRecordRevision {
    pub fn new(value: u64) -> Result<Self, ResidentSandboxCatalogError> {
        if !(1..=MAX_RESIDENT_SANDBOX_REVISION).contains(&value) {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident record revision is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResidentSandboxOperationGeneration(u64);

impl ResidentSandboxOperationGeneration {
    fn new(value: u64) -> Result<Self, ResidentSandboxCatalogError> {
        if !(1..=MAX_RESIDENT_SANDBOX_GENERATION).contains(&value) {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident operation generation is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxHostBinding {
    identity_digest: Sha256Digest,
    config_digest: Sha256Digest,
}

impl ResidentSandboxHostBinding {
    /// This is an identity-only declaration.  It is not a current host observation or runtime
    /// capability and cannot be used to invoke Lima or guest control.
    pub(crate) fn new(
        identity_digest: Sha256Digest,
        config_digest: Sha256Digest,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        Ok(Self {
            identity_digest,
            config_digest,
        })
    }

    #[must_use]
    pub const fn identity_digest(&self) -> &Sha256Digest {
        &self.identity_digest
    }

    #[must_use]
    pub const fn config_digest(&self) -> &Sha256Digest {
        &self.config_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResidentSandboxPhysicalState {
    Unmaterialized,
    StoppedBound { host: ResidentSandboxHostBinding },
    RunningBound { host: ResidentSandboxHostBinding },
    RevalidateRequired,
    Quarantined,
    RetireRequested,
    Retired,
}

impl ResidentSandboxPhysicalState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Retired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentSandboxOperationPhase {
    Authorized,
    Started,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ResidentSandboxActiveOperation {
    None,
    Materialize {
        generation: ResidentSandboxOperationGeneration,
        policy_identity: Sha256Digest,
        phase: ResidentSandboxOperationPhase,
    },
    Start {
        generation: ResidentSandboxOperationGeneration,
        policy_identity: Sha256Digest,
        phase: ResidentSandboxOperationPhase,
    },
    Stop {
        generation: ResidentSandboxOperationGeneration,
        policy_identity: Sha256Digest,
        phase: ResidentSandboxOperationPhase,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSandboxCheckpoint {
    MaterializeAuthorized,
    MaterializePrestartFailed,
    MaterializeStarted,
    MaterializeRecoveryRequired,
    StartAuthorized,
    StartPrestartFailed,
    StartStarted,
    StartRecoveryRequired,
    StopAuthorized,
    StopPrestartFailed,
    StopStarted,
    StopRecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxRecord {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    generation: ResidentSandboxGeneration,
    revision: ResidentSandboxRecordRevision,
    source_identity: ProjectDiskLimaSourceIdentity,
    locator: ResidentSandboxLocator,
    config: ResidentSandboxConfig,
    config_digest: Sha256Digest,
    physical: ResidentSandboxPhysicalState,
    last_operation_generation: Option<ResidentSandboxOperationGeneration>,
    active_operation: ResidentSandboxActiveOperation,
}

impl ResidentSandboxRecord {
    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn generation(&self) -> ResidentSandboxGeneration {
        self.generation
    }

    #[must_use]
    pub const fn revision(&self) -> ResidentSandboxRecordRevision {
        self.revision
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn locator(&self) -> &ResidentSandboxLocator {
        &self.locator
    }

    #[must_use]
    pub const fn config(&self) -> &ResidentSandboxConfig {
        &self.config
    }

    #[must_use]
    pub const fn config_digest(&self) -> &Sha256Digest {
        &self.config_digest
    }

    #[must_use]
    pub const fn physical(&self) -> &ResidentSandboxPhysicalState {
        &self.physical
    }

    #[must_use]
    pub const fn active_operation(&self) -> &ResidentSandboxActiveOperation {
        &self.active_operation
    }

    #[must_use]
    pub const fn last_operation_generation(&self) -> Option<ResidentSandboxOperationGeneration> {
        self.last_operation_generation
    }

    /// Begin a materialization transaction.  This only produces a pure checkpoint and never
    /// invokes an external callback.
    fn begin_materialize(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::MaterializeAuthorized)
    }

    fn begin_start(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StartAuthorized)
    }

    fn begin_stop(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StopAuthorized)
    }

    fn checkpoint_materialize_started(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::MaterializeStarted)
    }

    fn checkpoint_materialize_prestart_failed(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::MaterializePrestartFailed)
    }

    fn checkpoint_materialize_recovery_required(
        &self,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::MaterializeRecoveryRequired)
    }

    fn checkpoint_start_started(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StartStarted)
    }

    fn checkpoint_start_prestart_failed(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StartPrestartFailed)
    }

    fn checkpoint_start_recovery_required(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StartRecoveryRequired)
    }

    fn checkpoint_stop_started(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StopStarted)
    }

    fn checkpoint_stop_prestart_failed(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StopPrestartFailed)
    }

    fn checkpoint_stop_recovery_required(&self) -> Result<Self, ResidentSandboxCatalogError> {
        self.apply_checkpoint(ResidentSandboxCheckpoint::StopRecoveryRequired)
    }

    fn apply_checkpoint(
        &self,
        checkpoint: ResidentSandboxCheckpoint,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        if self.physical.is_terminal() {
            return Err(error(
                ResidentSandboxCatalogErrorKind::Conflict,
                "terminal resident sandbox cannot advance a checkpoint",
            ));
        }
        let (next_operation, last_operation_generation) = match (
            checkpoint,
            &self.physical,
            &self.active_operation,
        ) {
            (
                ResidentSandboxCheckpoint::MaterializeAuthorized,
                ResidentSandboxPhysicalState::Unmaterialized,
                ResidentSandboxActiveOperation::None,
            ) => {
                let generation = self.next_operation_generation()?;
                (
                    ResidentSandboxActiveOperation::Materialize {
                        generation,
                        policy_identity: derive_operation_policy_identity(
                            self,
                            ResidentSandboxOperationKind::Materialize,
                            generation,
                        )?,
                        phase: ResidentSandboxOperationPhase::Authorized,
                    },
                    Some(generation),
                )
            }
            (
                ResidentSandboxCheckpoint::StartAuthorized,
                ResidentSandboxPhysicalState::StoppedBound { .. },
                ResidentSandboxActiveOperation::None,
            ) => {
                let generation = self.next_operation_generation()?;
                (
                    ResidentSandboxActiveOperation::Start {
                        generation,
                        policy_identity: derive_operation_policy_identity(
                            self,
                            ResidentSandboxOperationKind::Start,
                            generation,
                        )?,
                        phase: ResidentSandboxOperationPhase::Authorized,
                    },
                    Some(generation),
                )
            }
            (
                ResidentSandboxCheckpoint::StopAuthorized,
                ResidentSandboxPhysicalState::RunningBound { .. },
                ResidentSandboxActiveOperation::None,
            ) => {
                let generation = self.next_operation_generation()?;
                (
                    ResidentSandboxActiveOperation::Stop {
                        generation,
                        policy_identity: derive_operation_policy_identity(
                            self,
                            ResidentSandboxOperationKind::Stop,
                            generation,
                        )?,
                        phase: ResidentSandboxOperationPhase::Authorized,
                    },
                    Some(generation),
                )
            }
            (
                ResidentSandboxCheckpoint::MaterializePrestartFailed,
                ResidentSandboxPhysicalState::Unmaterialized,
                ResidentSandboxActiveOperation::Materialize {
                    phase: ResidentSandboxOperationPhase::Authorized,
                    ..
                },
            ) => (
                ResidentSandboxActiveOperation::None,
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::MaterializeStarted,
                ResidentSandboxPhysicalState::Unmaterialized,
                ResidentSandboxActiveOperation::Materialize {
                    generation,
                    policy_identity,
                    phase: ResidentSandboxOperationPhase::Authorized,
                },
            ) => (
                ResidentSandboxActiveOperation::Materialize {
                    generation: *generation,
                    policy_identity: policy_identity.clone(),
                    phase: ResidentSandboxOperationPhase::Started,
                },
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::MaterializeRecoveryRequired,
                ResidentSandboxPhysicalState::Unmaterialized,
                ResidentSandboxActiveOperation::Materialize {
                    generation,
                    policy_identity,
                    phase: ResidentSandboxOperationPhase::Started,
                },
            ) => (
                ResidentSandboxActiveOperation::Materialize {
                    generation: *generation,
                    policy_identity: policy_identity.clone(),
                    phase: ResidentSandboxOperationPhase::RecoveryRequired,
                },
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::StartPrestartFailed,
                ResidentSandboxPhysicalState::StoppedBound { .. },
                ResidentSandboxActiveOperation::Start {
                    phase: ResidentSandboxOperationPhase::Authorized,
                    ..
                },
            ) => (
                ResidentSandboxActiveOperation::None,
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::StartStarted,
                ResidentSandboxPhysicalState::StoppedBound { .. },
                ResidentSandboxActiveOperation::Start {
                    generation,
                    policy_identity,
                    phase: ResidentSandboxOperationPhase::Authorized,
                },
            ) => (
                ResidentSandboxActiveOperation::Start {
                    generation: *generation,
                    policy_identity: policy_identity.clone(),
                    phase: ResidentSandboxOperationPhase::Started,
                },
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::StartRecoveryRequired,
                ResidentSandboxPhysicalState::StoppedBound { .. },
                ResidentSandboxActiveOperation::Start {
                    generation,
                    policy_identity,
                    phase: ResidentSandboxOperationPhase::Started,
                },
            ) => (
                ResidentSandboxActiveOperation::Start {
                    generation: *generation,
                    policy_identity: policy_identity.clone(),
                    phase: ResidentSandboxOperationPhase::RecoveryRequired,
                },
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::StopPrestartFailed,
                ResidentSandboxPhysicalState::RunningBound { .. },
                ResidentSandboxActiveOperation::Stop {
                    phase: ResidentSandboxOperationPhase::Authorized,
                    ..
                },
            ) => (
                ResidentSandboxActiveOperation::None,
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::StopStarted,
                ResidentSandboxPhysicalState::RunningBound { .. },
                ResidentSandboxActiveOperation::Stop {
                    generation,
                    policy_identity,
                    phase: ResidentSandboxOperationPhase::Authorized,
                },
            ) => (
                ResidentSandboxActiveOperation::Stop {
                    generation: *generation,
                    policy_identity: policy_identity.clone(),
                    phase: ResidentSandboxOperationPhase::Started,
                },
                self.last_operation_generation,
            ),
            (
                ResidentSandboxCheckpoint::StopRecoveryRequired,
                ResidentSandboxPhysicalState::RunningBound { .. },
                ResidentSandboxActiveOperation::Stop {
                    generation,
                    policy_identity,
                    phase: ResidentSandboxOperationPhase::Started,
                },
            ) => (
                ResidentSandboxActiveOperation::Stop {
                    generation: *generation,
                    policy_identity: policy_identity.clone(),
                    phase: ResidentSandboxOperationPhase::RecoveryRequired,
                },
                self.last_operation_generation,
            ),
            _ => {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                    "resident checkpoint does not exactly follow the current physical/operation state",
                ));
            }
        };
        let revision = ResidentSandboxRecordRevision::new(
            self.revision.get().checked_add(1).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Conflict,
                    "resident record revision cannot advance",
                )
            })?,
        )?;
        Ok(Self {
            revision,
            last_operation_generation,
            active_operation: next_operation,
            ..self.clone()
        })
    }

    fn next_operation_generation(
        &self,
    ) -> Result<ResidentSandboxOperationGeneration, ResidentSandboxCatalogError> {
        ResidentSandboxOperationGeneration::new(
            self.last_operation_generation
                .map_or(1, |generation| generation.get().saturating_add(1)),
        )
    }

    fn key(&self) -> ResidentSandboxKey {
        ResidentSandboxKey {
            project: self.project.clone(),
            sandbox_id: self.sandbox_id.clone(),
            generation: self.generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ResidentSandboxKey {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    generation: ResidentSandboxGeneration,
}

impl ResidentSandboxKey {
    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn generation(&self) -> ResidentSandboxGeneration {
        self.generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxAcceptanceRequest {
    request_id: ResidentSandboxAcceptanceRequestId,
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    source_identity: ProjectDiskLimaSourceIdentity,
    locator_policy_generation: ResidentLocatorPolicyGeneration,
    config: ResidentSandboxConfig,
}

impl ResidentSandboxAcceptanceRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: ResidentSandboxAcceptanceRequestId,
        project: ProjectIdentity,
        sandbox_id: ResidentSandboxId,
        source_identity: ProjectDiskLimaSourceIdentity,
        locator_policy_generation: ResidentLocatorPolicyGeneration,
        config: ResidentSandboxConfig,
    ) -> Self {
        Self {
            request_id,
            project,
            sandbox_id,
            source_identity,
            locator_policy_generation,
            config,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> &ResidentSandboxAcceptanceRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn locator_policy_generation(&self) -> ResidentLocatorPolicyGeneration {
        self.locator_policy_generation
    }

    #[must_use]
    pub const fn config(&self) -> &ResidentSandboxConfig {
        &self.config
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentSandboxAcceptanceDisposition {
    Created,
    Satisfied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxAcceptanceReceipt {
    disposition: ResidentSandboxAcceptanceDisposition,
    catalog_revision: ResidentSandboxCatalogRevision,
    key: ResidentSandboxKey,
    locator: ResidentSandboxLocator,
}

impl ResidentSandboxAcceptanceReceipt {
    #[must_use]
    pub const fn disposition(&self) -> ResidentSandboxAcceptanceDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn catalog_revision(&self) -> ResidentSandboxCatalogRevision {
        self.catalog_revision
    }

    #[must_use]
    pub const fn key(&self) -> &ResidentSandboxKey {
        &self.key
    }

    #[must_use]
    pub const fn locator(&self) -> &ResidentSandboxLocator {
        &self.locator
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AcceptanceRequestClaim {
    request_id: ResidentSandboxAcceptanceRequestId,
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    source_identity: ProjectDiskLimaSourceIdentity,
    locator_policy_generation: ResidentLocatorPolicyGeneration,
    config_digest: Sha256Digest,
    generation: ResidentSandboxGeneration,
    locator: ResidentSandboxLocator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GenerationMaximum {
    lineage: ResidentSandboxLineage,
    maximum: ResidentSandboxGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LocatorClaim {
    source_identity: ProjectDiskLimaSourceIdentity,
    locator: ResidentSandboxLocator,
    key: ResidentSandboxKey,
}

/// Pure catalog document.  It has no store/lock implementation and retains all claims needed to
/// prevent generation, request, and locator reuse after a later persistence slice is added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxCatalog {
    schema_version: u8,
    revision: ResidentSandboxCatalogRevision,
    acceptance_requests: Vec<AcceptanceRequestClaim>,
    generation_maxima: Vec<GenerationMaximum>,
    locator_claims: Vec<LocatorClaim>,
    entries: Vec<ResidentSandboxRecord>,
}

/// Name used by the persistence slice and callers that prefer the document terminology.
pub type ResidentSandboxCatalogDocument = ResidentSandboxCatalog;

impl ResidentSandboxCatalog {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            schema_version: RESIDENT_SANDBOX_CATALOG_SCHEMA_VERSION,
            revision: ResidentSandboxCatalogRevision(1),
            acceptance_requests: Vec::new(),
            generation_maxima: Vec::new(),
            locator_claims: Vec::new(),
            entries: Vec::new(),
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> ResidentSandboxCatalogRevision {
        self.revision
    }

    #[must_use]
    pub fn entries(&self) -> &[ResidentSandboxRecord] {
        &self.entries
    }

    #[must_use]
    pub fn find(&self, key: &ResidentSandboxKey) -> Option<&ResidentSandboxRecord> {
        self.entries.iter().find(|entry| entry.key() == *key)
    }

    /// Accept one request, allocating the next permanent generation and a controller-derived
    /// locator.  Repeating the exact request is satisfied without revision/generation movement;
    /// reusing its request ID with any changed identity/config is a conflict.
    pub fn accept(
        &self,
        request: ResidentSandboxAcceptanceRequest,
    ) -> Result<(Self, ResidentSandboxAcceptanceReceipt), ResidentSandboxCatalogError> {
        self.validate()?;
        let config_digest = request.config.digest()?;
        if request
            .config
            .project_disk
            .as_ref()
            .is_some_and(|disk| disk.source_identity != request.source_identity)
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident project-disk declaration uses a different Lima source",
            ));
        }
        if let Some(existing) = self
            .acceptance_requests
            .iter()
            .find(|claim| claim.request_id == request.request_id)
        {
            if existing.project == request.project
                && existing.sandbox_id == request.sandbox_id
                && existing.source_identity == request.source_identity
                && existing.locator_policy_generation == request.locator_policy_generation
                && existing.config_digest == config_digest
            {
                let key = ResidentSandboxKey {
                    project: existing.project.clone(),
                    sandbox_id: existing.sandbox_id.clone(),
                    generation: existing.generation,
                };
                return Ok((
                    self.clone(),
                    ResidentSandboxAcceptanceReceipt {
                        disposition: ResidentSandboxAcceptanceDisposition::Satisfied,
                        catalog_revision: self.revision,
                        key,
                        locator: existing.locator.clone(),
                    },
                ));
            }
            return Err(error(
                ResidentSandboxCatalogErrorKind::Conflict,
                "acceptance request ID is already bound to different resident evidence",
            ));
        }
        if self.acceptance_requests.len() >= MAX_RESIDENT_SANDBOX_ACCEPTANCE_REQUESTS
            || self.entries.len() >= MAX_RESIDENT_SANDBOX_ENTRIES
            || self.locator_claims.len() >= MAX_RESIDENT_SANDBOX_LOCATOR_CLAIMS
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::LimitExceeded,
                "resident catalog acceptance bound is exhausted",
            ));
        }
        let lineage = ResidentSandboxLineage {
            project: request.project.clone(),
            sandbox_id: request.sandbox_id.clone(),
        };
        if !self
            .generation_maxima
            .iter()
            .any(|maximum| maximum.lineage == lineage)
            && self.generation_maxima.len() >= MAX_RESIDENT_SANDBOX_LINEAGES
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::LimitExceeded,
                "resident catalog lineage bound is exhausted",
            ));
        }
        let generation = self
            .generation_maxima
            .iter()
            .find(|maximum| maximum.lineage == lineage)
            .map_or(Ok(1), |maximum| {
                maximum.maximum.get().checked_add(1).ok_or_else(|| {
                    error(
                        ResidentSandboxCatalogErrorKind::Conflict,
                        "resident sandbox generation cannot advance",
                    )
                })
            })?;
        if generation > MAX_RESIDENT_SANDBOX_GENERATION {
            return Err(error(
                ResidentSandboxCatalogErrorKind::LimitExceeded,
                "resident sandbox generation bound is exhausted",
            ));
        }
        let generation = ResidentSandboxGeneration::new(generation).map_err(|_| {
            error(
                ResidentSandboxCatalogErrorKind::Conflict,
                "resident sandbox generation cannot advance",
            )
        })?;
        let locator = derive_locator(
            &request.project,
            &request.sandbox_id,
            generation,
            &request.source_identity,
            request.locator_policy_generation,
            request.config.trust_class,
            request.config.lima_layout_policy_generation,
        )?;
        if self.locator_claims.iter().any(|claim| {
            claim.source_identity == request.source_identity && claim.locator == locator
        }) {
            return Err(error(
                ResidentSandboxCatalogErrorKind::Conflict,
                "derived Lima locator is already permanently claimed",
            ));
        }
        let key = ResidentSandboxKey {
            project: request.project.clone(),
            sandbox_id: request.sandbox_id.clone(),
            generation,
        };
        let record = ResidentSandboxRecord {
            project: request.project.clone(),
            sandbox_id: request.sandbox_id.clone(),
            generation,
            revision: ResidentSandboxRecordRevision(1),
            source_identity: request.source_identity.clone(),
            locator: locator.clone(),
            config: request.config,
            config_digest: config_digest.clone(),
            physical: ResidentSandboxPhysicalState::Unmaterialized,
            last_operation_generation: None,
            active_operation: ResidentSandboxActiveOperation::None,
        };
        let claim = AcceptanceRequestClaim {
            request_id: request.request_id,
            project: request.project,
            sandbox_id: request.sandbox_id,
            source_identity: request.source_identity,
            locator_policy_generation: request.locator_policy_generation,
            config_digest,
            generation,
            locator: locator.clone(),
        };
        let mut next = self.clone();
        next.revision = ResidentSandboxCatalogRevision::new(
            self.revision.get().checked_add(1).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Conflict,
                    "resident catalog revision cannot advance",
                )
            })?,
        )?;
        insert_sorted(&mut next.acceptance_requests, claim, |value| {
            value.request_id.clone()
        });
        if let Some(maximum) = next
            .generation_maxima
            .iter_mut()
            .find(|maximum| maximum.lineage == lineage)
        {
            maximum.maximum = generation;
        } else {
            insert_sorted(
                &mut next.generation_maxima,
                GenerationMaximum {
                    lineage,
                    maximum: generation,
                },
                |value| value.lineage.clone(),
            );
        }
        insert_sorted(
            &mut next.locator_claims,
            LocatorClaim {
                source_identity: record.source_identity.clone(),
                locator: locator.clone(),
                key: key.clone(),
            },
            |value| value.locator.clone(),
        );
        insert_sorted(&mut next.entries, record, ResidentSandboxRecord::key);
        next.validate()?;
        encode_resident_sandbox_catalog(&next)?;
        Ok((
            next.clone(),
            ResidentSandboxAcceptanceReceipt {
                disposition: ResidentSandboxAcceptanceDisposition::Created,
                catalog_revision: next.revision,
                key,
                locator,
            },
        ))
    }

    /// Apply one pure checkpoint to one exact record revision.  Canonical Started checkpoints are
    /// intentionally no-replay markers; this API has no callback or execution side effect.
    pub fn checkpoint(
        &self,
        key: &ResidentSandboxKey,
        expected_record_revision: ResidentSandboxRecordRevision,
        checkpoint: ResidentSandboxCheckpoint,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        let current = self.find(key).ok_or_else(|| {
            error(
                ResidentSandboxCatalogErrorKind::Missing,
                "resident sandbox generation is not catalogued",
            )
        })?;
        if current.revision != expected_record_revision {
            return Err(error(
                ResidentSandboxCatalogErrorKind::Conflict,
                "resident sandbox record revision is stale",
            ));
        }
        let successor = match checkpoint {
            ResidentSandboxCheckpoint::MaterializeAuthorized => current.begin_materialize()?,
            ResidentSandboxCheckpoint::MaterializePrestartFailed => {
                current.checkpoint_materialize_prestart_failed()?
            }
            ResidentSandboxCheckpoint::MaterializeStarted => {
                current.checkpoint_materialize_started()?
            }
            ResidentSandboxCheckpoint::MaterializeRecoveryRequired => {
                current.checkpoint_materialize_recovery_required()?
            }
            ResidentSandboxCheckpoint::StartAuthorized => current.begin_start()?,
            ResidentSandboxCheckpoint::StartPrestartFailed => {
                current.checkpoint_start_prestart_failed()?
            }
            ResidentSandboxCheckpoint::StartStarted => current.checkpoint_start_started()?,
            ResidentSandboxCheckpoint::StartRecoveryRequired => {
                current.checkpoint_start_recovery_required()?
            }
            ResidentSandboxCheckpoint::StopAuthorized => current.begin_stop()?,
            ResidentSandboxCheckpoint::StopPrestartFailed => {
                current.checkpoint_stop_prestart_failed()?
            }
            ResidentSandboxCheckpoint::StopStarted => current.checkpoint_stop_started()?,
            ResidentSandboxCheckpoint::StopRecoveryRequired => {
                current.checkpoint_stop_recovery_required()?
            }
        };
        let mut next = self.clone();
        let index = next
            .entries
            .iter()
            .position(|entry| entry.key() == *key)
            .ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Missing,
                    "resident sandbox generation disappeared",
                )
            })?;
        next.entries[index] = successor;
        next.revision = ResidentSandboxCatalogRevision::new(
            self.revision.get().checked_add(1).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Conflict,
                    "resident catalog revision cannot advance",
                )
            })?,
        )?;
        next.validate_successor_of(self)?;
        encode_resident_sandbox_catalog(&next)?;
        Ok(next)
    }

    /// Build a canonical bound-state fixture for sibling persistence tests. This constructor is
    /// absent from production builds because first host binding requires later fresh observation.
    #[cfg(test)]
    pub(crate) fn test_with_bound_physical_state(
        &self,
        key: &ResidentSandboxKey,
        running: bool,
    ) -> Result<Self, ResidentSandboxCatalogError> {
        let mut next = self.clone();
        let entry = next
            .entries
            .iter_mut()
            .find(|entry| entry.key() == *key)
            .ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Missing,
                    "resident sandbox generation is not catalogued",
                )
            })?;
        if entry.physical != ResidentSandboxPhysicalState::Unmaterialized
            || entry.active_operation != ResidentSandboxActiveOperation::None
            || entry.last_operation_generation.is_some()
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::Conflict,
                "resident sandbox test fixture is already lifecycle-bound",
            ));
        }
        entry.revision = ResidentSandboxRecordRevision::new(
            entry.revision.get().checked_add(1).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Conflict,
                    "resident record revision cannot advance",
                )
            })?,
        )?;
        let host = ResidentSandboxHostBinding::new(
            Sha256Digest::parse(&format!("sha256:{}", "e".repeat(64)))
                .expect("fixed test host digest"),
            entry.config_digest.clone(),
        )?;
        entry.physical = if running {
            ResidentSandboxPhysicalState::RunningBound { host }
        } else {
            ResidentSandboxPhysicalState::StoppedBound { host }
        };
        next.revision = ResidentSandboxCatalogRevision::new(
            next.revision.get().checked_add(1).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::Conflict,
                    "resident catalog revision cannot advance",
                )
            })?,
        )?;
        next.validate()?;
        encode_resident_sandbox_catalog(&next)?;
        Ok(next)
    }

    /// Validate a canonical one-step catalog successor.  This is suitable for a later atomic
    /// persistence boundary but performs no persistence itself.
    pub fn validate_successor_of(&self, current: &Self) -> Result<(), ResidentSandboxCatalogError> {
        current.validate()?;
        self.validate()?;
        if self.revision.get()
            != current.revision.get().checked_add(1).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                    "resident catalog revision cannot advance",
                )
            })?
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                "resident catalog successor revision is not exactly one step",
            ));
        }
        if self.acceptance_requests != current.acceptance_requests
            || self.generation_maxima != current.generation_maxima
            || self.locator_claims != current.locator_claims
        {
            if !is_exact_acceptance_successor(current, self) {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                    "resident catalog acceptance successor changes the wrong permanent claims",
                ));
            }
            return Ok(());
        }
        if self.entries.len() != current.entries.len() {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                "resident catalog entry count changed without acceptance",
            ));
        }
        let mut changes = self
            .entries
            .iter()
            .zip(&current.entries)
            .filter(|(next, prior)| next != prior);
        let Some((next, prior)) = changes.next() else {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                "resident catalog successor has no semantic change",
            ));
        };
        if changes.next().is_some()
            || next.key() != prior.key()
            || next.project != prior.project
            || next.sandbox_id != prior.sandbox_id
            || next.generation != prior.generation
            || next.source_identity != prior.source_identity
            || next.locator != prior.locator
            || next.config != prior.config
            || next.config_digest != prior.config_digest
            || next.physical != prior.physical
            || next.revision.get() != prior.revision.get() + 1
            || !valid_operation_successor(prior, next)
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::InvalidSuccessor,
                "resident catalog record is not an exact checkpoint successor",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ResidentSandboxCatalogError> {
        if self.schema_version != RESIDENT_SANDBOX_CATALOG_SCHEMA_VERSION
            || self.acceptance_requests.len() > MAX_RESIDENT_SANDBOX_ACCEPTANCE_REQUESTS
            || self.generation_maxima.len() > MAX_RESIDENT_SANDBOX_LINEAGES
            || self.locator_claims.len() > MAX_RESIDENT_SANDBOX_LOCATOR_CLAIMS
            || self.entries.len() > MAX_RESIDENT_SANDBOX_ENTRIES
            || self.acceptance_requests.len() != self.entries.len()
            || self.locator_claims.len() != self.entries.len()
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::CorruptState,
                "resident catalog schema or collection bound is invalid",
            ));
        }
        let represented = 1_u64
            .checked_add(self.acceptance_requests.len() as u64)
            .and_then(|value| {
                value.checked_add(
                    self.entries
                        .iter()
                        .map(|entry| entry.revision.get().saturating_sub(1))
                        .sum::<u64>(),
                )
            })
            .ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "resident catalog represented revision overflows",
                )
            })?;
        if self.revision.get() != represented {
            return Err(error(
                ResidentSandboxCatalogErrorKind::CorruptState,
                "resident catalog revision conflicts with represented history",
            ));
        }
        if !is_sorted_unique(&self.acceptance_requests, |value| value.request_id.clone())
            || !is_sorted_unique(&self.generation_maxima, |value| value.lineage.clone())
            || !is_sorted_unique(&self.locator_claims, |value| value.locator.clone())
            || !is_sorted_unique(&self.entries, ResidentSandboxRecord::key)
        {
            return Err(error(
                ResidentSandboxCatalogErrorKind::Duplicate,
                "resident catalog contains unsorted or duplicate permanent identity claims",
            ));
        }
        let mut keys = BTreeSet::new();
        let mut locators = BTreeSet::new();
        for entry in &self.entries {
            validate_record(entry)?;
            let key = entry.key();
            if !keys.insert(key.clone())
                || !locators.insert((entry.source_identity.clone(), entry.locator.clone()))
            {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::Duplicate,
                    "resident catalog contains duplicate generation, lineage, or locator identity",
                ));
            }
            let maximum = self
                .generation_maxima
                .iter()
                .find(|maximum| {
                    maximum.lineage.project == entry.project
                        && maximum.lineage.sandbox_id == entry.sandbox_id
                })
                .ok_or_else(|| {
                    error(
                        ResidentSandboxCatalogErrorKind::CorruptState,
                        "resident entry has no permanent generation maximum",
                    )
                })?;
            if entry.generation > maximum.maximum {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "resident entry exceeds its generation maximum",
                ));
            }
            let claim = self
                .locator_claims
                .iter()
                .find(|claim| claim.key == key)
                .ok_or_else(|| {
                    error(
                        ResidentSandboxCatalogErrorKind::CorruptState,
                        "resident entry has no permanent locator claim",
                    )
                })?;
            if claim.source_identity != entry.source_identity || claim.locator != entry.locator {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "resident locator claim conflicts with entry",
                ));
            }
        }
        let mut locator_keys = BTreeSet::new();
        for claim in &self.locator_claims {
            let entry = self.find(&claim.key).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "resident locator claim has no retained generation",
                )
            })?;
            if !locator_keys.insert(claim.key.clone())
                || claim.source_identity != entry.source_identity
                || claim.locator != entry.locator
            {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::Duplicate,
                    "resident locator claim conflicts with retained generation history",
                ));
            }
        }
        let mut acceptance_keys = BTreeSet::new();
        for claim in &self.acceptance_requests {
            let key = ResidentSandboxKey {
                project: claim.project.clone(),
                sandbox_id: claim.sandbox_id.clone(),
                generation: claim.generation,
            };
            let entry = self.find(&key).ok_or_else(|| {
                error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "acceptance request has no resident entry",
                )
            })?;
            if !acceptance_keys.insert(key)
                || entry.source_identity != claim.source_identity
                || entry.locator != claim.locator
                || entry.config_digest != claim.config_digest
                || derive_locator(
                    &claim.project,
                    &claim.sandbox_id,
                    claim.generation,
                    &claim.source_identity,
                    claim.locator_policy_generation,
                    entry.config.trust_class,
                    entry.config.lima_layout_policy_generation,
                )? != claim.locator
            {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "acceptance request conflicts with resident entry",
                ));
            }
        }
        for maximum in &self.generation_maxima {
            let observed_maximum = self
                .entries
                .iter()
                .filter(|entry| {
                    entry.project == maximum.lineage.project
                        && entry.sandbox_id == maximum.lineage.sandbox_id
                })
                .map(|entry| entry.generation)
                .max();
            if maximum.maximum.get() > MAX_RESIDENT_SANDBOX_GENERATION
                || observed_maximum != Some(maximum.maximum)
            {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "resident generation maximum conflicts with retained history",
                ));
            }
        }
        Ok(())
    }
}

fn is_exact_acceptance_successor(
    current: &ResidentSandboxCatalog,
    next: &ResidentSandboxCatalog,
) -> bool {
    if next.acceptance_requests.len() != current.acceptance_requests.len() + 1
        || next.locator_claims.len() != current.locator_claims.len() + 1
        || next.entries.len() != current.entries.len() + 1
        || !current.entries.iter().all(|entry| {
            next.find(&entry.key())
                .is_some_and(|candidate| candidate == entry)
        })
        || !current.acceptance_requests.iter().all(|request| {
            next.acceptance_requests
                .iter()
                .find(|candidate| candidate.request_id == request.request_id)
                == Some(request)
        })
        || !current.locator_claims.iter().all(|claim| {
            next.locator_claims
                .iter()
                .find(|candidate| candidate.key == claim.key)
                == Some(claim)
        })
    {
        return false;
    }

    let mut added_entries = next
        .entries
        .iter()
        .filter(|entry| current.find(&entry.key()).is_none());
    let Some(entry) = added_entries.next() else {
        return false;
    };
    if added_entries.next().is_some() {
        return false;
    }

    let mut added_requests = next.acceptance_requests.iter().filter(|request| {
        !current
            .acceptance_requests
            .iter()
            .any(|candidate| candidate.request_id == request.request_id)
    });
    let Some(request) = added_requests.next() else {
        return false;
    };
    if added_requests.next().is_some() {
        return false;
    }

    let mut added_locators = next.locator_claims.iter().filter(|claim| {
        !current
            .locator_claims
            .iter()
            .any(|candidate| candidate.key == claim.key)
    });
    let Some(locator) = added_locators.next() else {
        return false;
    };
    if added_locators.next().is_some()
        || entry.revision.get() != 1
        || entry.physical != ResidentSandboxPhysicalState::Unmaterialized
        || entry.last_operation_generation.is_some()
        || entry.active_operation != ResidentSandboxActiveOperation::None
        || request.project != entry.project
        || request.sandbox_id != entry.sandbox_id
        || request.source_identity != entry.source_identity
        || request.config_digest != entry.config_digest
        || request.generation != entry.generation
        || request.locator != entry.locator
        || locator.key != entry.key()
        || locator.source_identity != entry.source_identity
        || locator.locator != entry.locator
    {
        return false;
    }

    let lineage = ResidentSandboxLineage {
        project: entry.project.clone(),
        sandbox_id: entry.sandbox_id.clone(),
    };
    let prior_maximum = current
        .generation_maxima
        .iter()
        .find(|maximum| maximum.lineage == lineage);
    match prior_maximum {
        Some(prior) => {
            next.generation_maxima.len() == current.generation_maxima.len()
                && prior
                    .maximum
                    .get()
                    .checked_add(1)
                    .is_some_and(|generation| generation == entry.generation.get())
                && current.generation_maxima.iter().all(|maximum| {
                    next.generation_maxima
                        .iter()
                        .find(|candidate| candidate.lineage == maximum.lineage)
                        .is_some_and(|candidate| {
                            if maximum.lineage == lineage {
                                candidate.maximum == entry.generation
                            } else {
                                candidate == maximum
                            }
                        })
                })
        }
        None => {
            entry.generation.get() == 1
                && next.generation_maxima.len() == current.generation_maxima.len() + 1
                && current.generation_maxima.iter().all(|maximum| {
                    next.generation_maxima
                        .iter()
                        .any(|candidate| candidate == maximum)
                })
                && next.generation_maxima.iter().any(|maximum| {
                    maximum.lineage == lineage && maximum.maximum == entry.generation
                })
        }
    }
}

fn validate_record(record: &ResidentSandboxRecord) -> Result<(), ResidentSandboxCatalogError> {
    if record.generation.get() > MAX_RESIDENT_SANDBOX_GENERATION
        || record.config_digest != record.config.digest()?
        || record.config.trust_class != ResidentSandboxTrustClass::UltraTrustedProject
        || record.config.backend != ResidentSandboxBackend::Vz
        || record.config.architecture != ResidentSandboxArchitecture::Aarch64
        || record.config.auto_format
        || record.config.automount
        || record
            .config
            .project_disk
            .as_ref()
            .is_some_and(|disk| disk.source_identity != record.source_identity)
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident record config identity or required VZ policy is invalid",
        ));
    }
    if record.revision.get() == 1
        && (record.physical != ResidentSandboxPhysicalState::Unmaterialized
            || record.last_operation_generation.is_some()
            || record.active_operation != ResidentSandboxActiveOperation::None)
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "initial resident generation contains fabricated lifecycle history",
        ));
    }
    if record.locator.0.len() > MAX_RESIDENT_SANDBOX_LOCATOR_BYTES
        || !valid_locator(&record.locator.0)
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident Lima locator is not bounded canonical ASCII",
        ));
    }
    let active = match (&record.physical, &record.active_operation) {
        (_, ResidentSandboxActiveOperation::None) => None,
        (
            ResidentSandboxPhysicalState::Unmaterialized,
            ResidentSandboxActiveOperation::Materialize {
                generation,
                policy_identity,
                ..
            },
        ) => Some((
            ResidentSandboxOperationKind::Materialize,
            *generation,
            policy_identity,
        )),
        (
            ResidentSandboxPhysicalState::StoppedBound { .. },
            ResidentSandboxActiveOperation::Start {
                generation,
                policy_identity,
                ..
            },
        ) => Some((
            ResidentSandboxOperationKind::Start,
            *generation,
            policy_identity,
        )),
        (
            ResidentSandboxPhysicalState::RunningBound { .. },
            ResidentSandboxActiveOperation::Stop {
                generation,
                policy_identity,
                ..
            },
        ) => Some((
            ResidentSandboxOperationKind::Stop,
            *generation,
            policy_identity,
        )),
        _ => {
            return Err(error(
                ResidentSandboxCatalogErrorKind::CorruptState,
                "resident active operation does not match physical state",
            ));
        }
    };
    if let Some((kind, generation, policy_identity)) = active
        && (record.last_operation_generation != Some(generation)
            || generation.get() > MAX_RESIDENT_SANDBOX_GENERATION
            || derive_operation_policy_identity(record, kind, generation)? != *policy_identity)
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident active operation identity or generation is invalid",
        ));
    }
    if record.active_operation == ResidentSandboxActiveOperation::None
        && record
            .last_operation_generation
            .is_some_and(|generation| generation.get() > MAX_RESIDENT_SANDBOX_GENERATION)
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident operation generation exceeds its bound",
        ));
    }
    if let ResidentSandboxPhysicalState::StoppedBound { host }
    | ResidentSandboxPhysicalState::RunningBound { host } = &record.physical
        && host.config_digest != record.config_digest
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident host binding config identity drifted",
        ));
    }
    Ok(())
}

fn valid_operation_successor(prior: &ResidentSandboxRecord, next: &ResidentSandboxRecord) -> bool {
    match (&prior.active_operation, &next.active_operation) {
        (
            ResidentSandboxActiveOperation::Materialize {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
            ResidentSandboxActiveOperation::None,
        )
        | (
            ResidentSandboxActiveOperation::Start {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
            ResidentSandboxActiveOperation::None,
        )
        | (
            ResidentSandboxActiveOperation::Stop {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
            ResidentSandboxActiveOperation::None,
        ) => {
            prior.last_operation_generation == Some(*generation)
                && next.last_operation_generation == Some(*generation)
        }
        (
            ResidentSandboxActiveOperation::None,
            ResidentSandboxActiveOperation::Materialize {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
        ) => {
            next.last_operation_generation == Some(*generation)
                && generation.get()
                    == prior
                        .last_operation_generation
                        .map_or(1, |prior| prior.get().saturating_add(1))
        }
        (
            ResidentSandboxActiveOperation::None,
            ResidentSandboxActiveOperation::Start {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
        )
        | (
            ResidentSandboxActiveOperation::None,
            ResidentSandboxActiveOperation::Stop {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
        ) => {
            next.last_operation_generation == Some(*generation)
                && generation.get()
                    == prior
                        .last_operation_generation
                        .map_or(1, |prior| prior.get().saturating_add(1))
        }
        (
            ResidentSandboxActiveOperation::Materialize {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Authorized,
            },
            ResidentSandboxActiveOperation::Materialize {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
        )
        | (
            ResidentSandboxActiveOperation::Start {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Authorized,
            },
            ResidentSandboxActiveOperation::Start {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
        )
        | (
            ResidentSandboxActiveOperation::Stop {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Authorized,
            },
            ResidentSandboxActiveOperation::Stop {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
        ) => {
            prior_generation == next_generation
                && prior_policy == next_policy
                && prior.last_operation_generation == next.last_operation_generation
        }
        (
            ResidentSandboxActiveOperation::Materialize {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
            ResidentSandboxActiveOperation::Materialize {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
            },
        )
        | (
            ResidentSandboxActiveOperation::Start {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
            ResidentSandboxActiveOperation::Start {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
            },
        )
        | (
            ResidentSandboxActiveOperation::Stop {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
            ResidentSandboxActiveOperation::Stop {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
            },
        ) => {
            prior_generation == next_generation
                && prior_policy == next_policy
                && prior.last_operation_generation == next.last_operation_generation
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentSandboxOperationKind {
    Materialize,
    Start,
    Stop,
}

fn derive_operation_policy_identity(
    record: &ResidentSandboxRecord,
    kind: ResidentSandboxOperationKind,
    generation: ResidentSandboxOperationGeneration,
) -> Result<Sha256Digest, ResidentSandboxCatalogError> {
    let mut hasher = Sha256::new();
    hasher.update(OPERATION_POLICY_DOMAIN);
    hash_field(&mut hasher, record.project.as_str().as_bytes());
    hash_field(&mut hasher, record.sandbox_id.as_str().as_bytes());
    hash_field(&mut hasher, &record.generation.get().to_be_bytes());
    hash_field(
        &mut hasher,
        record.source_identity.digest().as_str().as_bytes(),
    );
    hash_field(&mut hasher, record.locator.as_str().as_bytes());
    hash_field(&mut hasher, record.config_digest.as_str().as_bytes());
    match &record.physical {
        ResidentSandboxPhysicalState::StoppedBound { host }
        | ResidentSandboxPhysicalState::RunningBound { host } => {
            hash_field(&mut hasher, b"host_bound");
            hash_field(&mut hasher, host.identity_digest.as_str().as_bytes());
            hash_field(&mut hasher, host.config_digest.as_str().as_bytes());
        }
        _ => hash_field(&mut hasher, b"host_unbound"),
    }
    hash_field(&mut hasher, &generation.get().to_be_bytes());
    hash_field(
        &mut hasher,
        match kind {
            ResidentSandboxOperationKind::Materialize => b"materialize",
            ResidentSandboxOperationKind::Start => b"start",
            ResidentSandboxOperationKind::Stop => b"stop",
        },
    );
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| {
        error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident operation policy identity could not be constructed",
        )
    })
}

fn derive_locator(
    project: &ProjectIdentity,
    sandbox_id: &ResidentSandboxId,
    generation: ResidentSandboxGeneration,
    source_identity: &ProjectDiskLimaSourceIdentity,
    policy_generation: ResidentLocatorPolicyGeneration,
    trust_class: ResidentSandboxTrustClass,
    lima_layout_policy_generation: ResidentLimaLayoutGeneration,
) -> Result<ResidentSandboxLocator, ResidentSandboxCatalogError> {
    let mut hasher = Sha256::new();
    hasher.update(LOCATOR_POLICY_DOMAIN);
    hash_field(&mut hasher, project.as_str().as_bytes());
    hash_field(&mut hasher, sandbox_id.as_str().as_bytes());
    hash_field(&mut hasher, &generation.get().to_be_bytes());
    hash_field(&mut hasher, source_identity.digest().as_str().as_bytes());
    hash_field(&mut hasher, &policy_generation.get().to_be_bytes());
    hash_field(
        &mut hasher,
        match trust_class {
            ResidentSandboxTrustClass::UltraTrustedProject => b"ultra_trusted_project",
        },
    );
    hash_field(
        &mut hasher,
        &lima_layout_policy_generation.get().to_be_bytes(),
    );
    let digest = format!("{:x}", hasher.finalize());
    let locator = format!("smolrunner-{}", &digest[..52]);
    if !valid_locator(&locator) {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "derived Lima locator is not canonical",
        ));
    }
    Ok(ResidentSandboxLocator(locator))
}

fn hash_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update((field.len() as u64).to_be_bytes());
    hasher.update(field);
}

fn valid_locator(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RESIDENT_SANDBOX_LOCATOR_BYTES
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_identifier(
    value: &str,
    label: &'static str,
) -> Result<(), ResidentSandboxCatalogError> {
    let Some(first) = value.bytes().next() else {
        return Err(error(ResidentSandboxCatalogErrorKind::InvalidInput, label));
    };
    if value.len() > MAX_IDENTIFIER_BYTES
        || !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b':')
        })
    {
        return Err(error(ResidentSandboxCatalogErrorKind::InvalidInput, label));
    }
    Ok(())
}

fn insert_sorted<T, K: Ord>(values: &mut Vec<T>, value: T, key: impl Fn(&T) -> K) {
    let index = values.partition_point(|current| key(current) < key(&value));
    values.insert(index, value);
}

fn is_sorted_unique<T, K: Ord>(values: &[T], key: impl Fn(&T) -> K) -> bool {
    values.windows(2).all(|pair| key(&pair[0]) < key(&pair[1]))
}

fn error(
    kind: ResidentSandboxCatalogErrorKind,
    message: &'static str,
) -> ResidentSandboxCatalogError {
    ResidentSandboxCatalogError { kind, message }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ResidentSandboxCatalogErrorKind {
    InvalidInput,
    Missing,
    Conflict,
    CorruptState,
    Duplicate,
    LimitExceeded,
    InvalidSuccessor,
    UnsupportedVersion,
    NonCanonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxCatalogError {
    kind: ResidentSandboxCatalogErrorKind,
    message: &'static str,
}

impl ResidentSandboxCatalogError {
    #[must_use]
    pub const fn kind(self) -> ResidentSandboxCatalogErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self.kind {
            ResidentSandboxCatalogErrorKind::InvalidInput => "invalid_input",
            ResidentSandboxCatalogErrorKind::Missing => "missing",
            ResidentSandboxCatalogErrorKind::Conflict => "conflict",
            ResidentSandboxCatalogErrorKind::CorruptState => "corrupt_state",
            ResidentSandboxCatalogErrorKind::Duplicate => "duplicate",
            ResidentSandboxCatalogErrorKind::LimitExceeded => "limit_exceeded",
            ResidentSandboxCatalogErrorKind::InvalidSuccessor => "invalid_successor",
            ResidentSandboxCatalogErrorKind::UnsupportedVersion => "unsupported_version",
            ResidentSandboxCatalogErrorKind::NonCanonical => "noncanonical",
        }
    }
}

impl fmt::Display for ResidentSandboxCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ResidentSandboxCatalogError {}

// Wire types are deliberately separate from the domain model.  Every object rejects unknown
// fields, and decode compares input bytes with this serializer so whitespace/order/number
// spellings and duplicate-field ambiguity cannot cross the catalog boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentSandboxCatalogWire {
    schema_version: u8,
    revision: u64,
    acceptance_requests: Vec<AcceptanceRequestWire>,
    generation_maxima: Vec<GenerationMaximumWire>,
    locator_claims: Vec<LocatorClaimWire>,
    entries: Vec<ResidentSandboxRecordWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceRequestWire {
    request_id: String,
    project: String,
    sandbox_id: String,
    source_identity: String,
    locator_policy_generation: u64,
    config_digest: String,
    generation: u64,
    locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationMaximumWire {
    project: String,
    sandbox_id: String,
    maximum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocatorClaimWire {
    source_identity: String,
    locator: String,
    project: String,
    sandbox_id: String,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentSandboxRecordWire {
    project: String,
    sandbox_id: String,
    generation: u64,
    revision: u64,
    source_identity: String,
    locator: String,
    config: ResidentSandboxConfigWire,
    config_digest: String,
    physical: ResidentSandboxPhysicalStateWire,
    last_operation_generation: Option<u64>,
    active_operation: ResidentSandboxActiveOperationWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentSandboxConfigWire {
    prepared_template_generation: u64,
    sandbox_config_generation: u64,
    trust_class: ResidentSandboxTrustClass,
    backend: ResidentSandboxBackend,
    architecture: ResidentSandboxArchitecture,
    lima_layout_policy_generation: u64,
    resources: ResidentResourceDeclarationWire,
    network_policy_generation: u64,
    credential_policy_generation: u64,
    guest_control_policy_generation: u64,
    guest_privilege_policy: ResidentGuestPrivilegePolicyWire,
    project_integration_policy_generation: u64,
    auto_format: bool,
    automount: bool,
    project_disk: Option<ResidentProjectDiskConfigBindingWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentResourceDeclarationWire {
    generation: u64,
    cpu_millis: u32,
    memory_bytes: u64,
    root_disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentGuestPrivilegePolicyWire {
    generation: u64,
    controller_account: String,
    controller_is_sole_normal_login: bool,
    controller_root_escalation: ResidentControllerRootEscalation,
    task_account: String,
    task_is_distinct_from_controller_and_root: bool,
    task_password_policy: ResidentTaskPasswordPolicy,
    task_supplementary_groups: Vec<String>,
    task_root_escalation: ResidentTaskRootEscalation,
    task_control_mutation: ResidentTaskControlMutation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentProjectDiskConfigBindingWire {
    source_identity: String,
    disk_id: String,
    disk_generation: u64,
    locator: String,
    create_provenance_identity: String,
    access: ResidentProjectDiskAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum ResidentSandboxPhysicalStateWire {
    Unmaterialized,
    StoppedBound {
        host: ResidentSandboxHostBindingWire,
    },
    RunningBound {
        host: ResidentSandboxHostBindingWire,
    },
    RevalidateRequired,
    Quarantined,
    RetireRequested,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentSandboxHostBindingWire {
    identity_digest: String,
    config_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ResidentSandboxActiveOperationWire {
    None,
    Materialize {
        generation: u64,
        policy_identity: String,
        phase: ResidentSandboxOperationPhase,
    },
    Start {
        generation: u64,
        policy_identity: String,
        phase: ResidentSandboxOperationPhase,
    },
    Stop {
        generation: u64,
        policy_identity: String,
        phase: ResidentSandboxOperationPhase,
    },
}

impl From<&ResidentSandboxConfig> for ResidentSandboxConfigWire {
    fn from(value: &ResidentSandboxConfig) -> Self {
        Self {
            prepared_template_generation: value.prepared_template_generation.get(),
            sandbox_config_generation: value.sandbox_config_generation.get(),
            trust_class: value.trust_class,
            backend: value.backend,
            architecture: value.architecture,
            lima_layout_policy_generation: value.lima_layout_policy_generation.get(),
            resources: ResidentResourceDeclarationWire {
                generation: value.resources.generation.get(),
                cpu_millis: value.resources.cpu_millis,
                memory_bytes: value.resources.memory_bytes,
                root_disk_bytes: value.resources.root_disk_bytes,
            },
            network_policy_generation: value.network_policy_generation.get(),
            credential_policy_generation: value.credential_policy_generation.get(),
            guest_control_policy_generation: value.guest_control_policy_generation.get(),
            guest_privilege_policy: ResidentGuestPrivilegePolicyWire {
                generation: value.guest_privilege_policy.generation.get(),
                controller_account: value.guest_privilege_policy.controller_account.clone(),
                controller_is_sole_normal_login: value
                    .guest_privilege_policy
                    .controller_is_sole_normal_login,
                controller_root_escalation: value.guest_privilege_policy.controller_root_escalation,
                task_account: value.guest_privilege_policy.task_account.clone(),
                task_is_distinct_from_controller_and_root: value
                    .guest_privilege_policy
                    .task_is_distinct_from_controller_and_root,
                task_password_policy: value.guest_privilege_policy.task_password_policy,
                task_supplementary_groups: value
                    .guest_privilege_policy
                    .task_supplementary_groups
                    .clone(),
                task_root_escalation: value.guest_privilege_policy.task_root_escalation,
                task_control_mutation: value.guest_privilege_policy.task_control_mutation,
            },
            project_integration_policy_generation: value
                .project_integration_policy_generation
                .get(),
            auto_format: value.auto_format,
            automount: value.automount,
            project_disk: value.project_disk.as_ref().map(|disk| {
                ResidentProjectDiskConfigBindingWire {
                    source_identity: disk.source_identity.digest().as_str().to_owned(),
                    disk_id: disk.disk_id.as_str().to_owned(),
                    disk_generation: disk.disk_generation.get(),
                    locator: disk.locator.as_str().to_owned(),
                    create_provenance_identity: disk.create_provenance_identity.as_str().to_owned(),
                    access: disk.access,
                }
            }),
        }
    }
}

fn decode_config(
    wire: ResidentSandboxConfigWire,
) -> Result<ResidentSandboxConfig, ResidentSandboxCatalogError> {
    if wire.trust_class != ResidentSandboxTrustClass::UltraTrustedProject
        || wire.backend != ResidentSandboxBackend::Vz
        || wire.architecture != ResidentSandboxArchitecture::Aarch64
        || wire.auto_format
        || wire.automount
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident trust/backend/architecture or project-disk mount policy is unreviewed",
        ));
    }
    let resources = ResidentResourceDeclaration::new(
        ResidentResourceGeneration::new(wire.resources.generation)?,
        wire.resources.cpu_millis,
        wire.resources.memory_bytes,
        wire.resources.root_disk_bytes,
    )?;
    let guest = wire.guest_privilege_policy;
    let policy = ResidentGuestPrivilegePolicy::reviewed(
        ResidentGuestPrivilegePolicyGeneration::new(guest.generation)?,
    );
    if guest.controller_account != policy.controller_account
        || guest.controller_is_sole_normal_login != policy.controller_is_sole_normal_login
        || guest.controller_root_escalation != policy.controller_root_escalation
        || guest.task_account != policy.task_account
        || guest.task_is_distinct_from_controller_and_root
            != policy.task_is_distinct_from_controller_and_root
        || guest.task_password_policy != policy.task_password_policy
        || guest.task_supplementary_groups != policy.task_supplementary_groups
        || guest.task_root_escalation != policy.task_root_escalation
        || guest.task_control_mutation != policy.task_control_mutation
    {
        return Err(error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "guest privilege declaration is not the reviewed V1 split",
        ));
    }
    let project_disk = wire
        .project_disk
        .map(|disk| {
            if disk.access != ResidentProjectDiskAccess::Writable {
                return Err(error(
                    ResidentSandboxCatalogErrorKind::CorruptState,
                    "resident project disk access is not the reviewed writable policy",
                ));
            }
            Ok(ResidentProjectDiskConfigBinding::new(
                ProjectDiskLimaSourceIdentity::parse(&disk.source_identity).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "project disk source identity is invalid",
                    )
                })?,
                ProjectDiskId::parse(&disk.disk_id).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "project disk ID is invalid",
                    )
                })?,
                ProjectDiskGeneration::new(disk.disk_generation).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "project disk generation is invalid",
                    )
                })?,
                LimaStandaloneDiskName::parse(&disk.locator).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "project disk locator is invalid",
                    )
                })?,
                Sha256Digest::parse(&disk.create_provenance_identity).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "project disk create provenance identity is invalid",
                    )
                })?,
            ))
        })
        .transpose()?;
    ResidentSandboxConfig::reviewed(
        ResidentPreparedTemplateGeneration::new(wire.prepared_template_generation)?,
        ResidentSandboxConfigGeneration::new(wire.sandbox_config_generation)?,
        ResidentLimaLayoutGeneration::new(wire.lima_layout_policy_generation)?,
        resources,
        ResidentNetworkPolicyGeneration::new(wire.network_policy_generation)?,
        ResidentCredentialPolicyGeneration::new(wire.credential_policy_generation)?,
        ResidentGuestControlPolicyGeneration::new(wire.guest_control_policy_generation)?,
        policy,
        ResidentProjectIntegrationPolicyGeneration::new(
            wire.project_integration_policy_generation,
        )?,
        project_disk,
    )
}

pub fn encode_resident_sandbox_catalog(
    catalog: &ResidentSandboxCatalog,
) -> Result<Vec<u8>, ResidentSandboxCatalogError> {
    catalog.validate()?;
    let wire = ResidentSandboxCatalogWire::from(catalog);
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident catalog could not be encoded",
        )
    })?;
    if bytes.len() > MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES {
        return Err(error(
            ResidentSandboxCatalogErrorKind::LimitExceeded,
            "resident catalog encoded document exceeds its bound",
        ));
    }
    Ok(bytes)
}

pub fn decode_resident_sandbox_catalog(
    bytes: &[u8],
) -> Result<ResidentSandboxCatalog, ResidentSandboxCatalogError> {
    if bytes.is_empty() || bytes.len() > MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES {
        return Err(error(
            ResidentSandboxCatalogErrorKind::LimitExceeded,
            "resident catalog document is empty or exceeds its bound",
        ));
    }
    let wire: ResidentSandboxCatalogWire = serde_json::from_slice(bytes).map_err(|_| {
        error(
            ResidentSandboxCatalogErrorKind::CorruptState,
            "resident catalog JSON is malformed, duplicated, or contains unknown fields",
        )
    })?;
    if wire.schema_version != RESIDENT_SANDBOX_CATALOG_SCHEMA_VERSION {
        return Err(error(
            ResidentSandboxCatalogErrorKind::UnsupportedVersion,
            "resident catalog schema version is unsupported",
        ));
    }
    let revision = ResidentSandboxCatalogRevision::new(wire.revision)?;
    let mut acceptance_requests = Vec::with_capacity(wire.acceptance_requests.len());
    for request in wire.acceptance_requests {
        acceptance_requests.push(AcceptanceRequestClaim {
            request_id: ResidentSandboxAcceptanceRequestId::parse(&request.request_id)?,
            project: ProjectIdentity::parse(&request.project).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident project identity is invalid",
                )
            })?,
            sandbox_id: ResidentSandboxId::parse(&request.sandbox_id).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident sandbox ID is invalid",
                )
            })?,
            source_identity: ProjectDiskLimaSourceIdentity::parse(&request.source_identity)
                .map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident Lima source identity is invalid",
                    )
                })?,
            locator_policy_generation: ResidentLocatorPolicyGeneration::new(
                request.locator_policy_generation,
            )?,
            config_digest: Sha256Digest::parse(&request.config_digest).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident config digest is invalid",
                )
            })?,
            generation: ResidentSandboxGeneration::new(request.generation).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident sandbox generation is invalid",
                )
            })?,
            locator: parse_locator(&request.locator)?,
        });
    }
    let mut generation_maxima = Vec::with_capacity(wire.generation_maxima.len());
    for maximum in wire.generation_maxima {
        generation_maxima.push(GenerationMaximum {
            lineage: ResidentSandboxLineage {
                project: ProjectIdentity::parse(&maximum.project).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident project identity is invalid",
                    )
                })?,
                sandbox_id: ResidentSandboxId::parse(&maximum.sandbox_id).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident sandbox ID is invalid",
                    )
                })?,
            },
            maximum: ResidentSandboxGeneration::new(maximum.maximum).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident generation maximum is invalid",
                )
            })?,
        });
    }
    let mut locator_claims = Vec::with_capacity(wire.locator_claims.len());
    for claim in wire.locator_claims {
        locator_claims.push(LocatorClaim {
            source_identity: ProjectDiskLimaSourceIdentity::parse(&claim.source_identity).map_err(
                |_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident Lima source identity is invalid",
                    )
                },
            )?,
            locator: parse_locator(&claim.locator)?,
            key: ResidentSandboxKey {
                project: ProjectIdentity::parse(&claim.project).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident project identity is invalid",
                    )
                })?,
                sandbox_id: ResidentSandboxId::parse(&claim.sandbox_id).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident sandbox ID is invalid",
                    )
                })?,
                generation: ResidentSandboxGeneration::new(claim.generation).map_err(|_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident sandbox generation is invalid",
                    )
                })?,
            },
        });
    }
    let mut entries = Vec::with_capacity(wire.entries.len());
    for entry in wire.entries {
        let config = decode_config(entry.config)?;
        let config_digest = Sha256Digest::parse(&entry.config_digest).map_err(|_| {
            error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident config digest is invalid",
            )
        })?;
        entries.push(ResidentSandboxRecord {
            project: ProjectIdentity::parse(&entry.project).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident project identity is invalid",
                )
            })?,
            sandbox_id: ResidentSandboxId::parse(&entry.sandbox_id).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident sandbox ID is invalid",
                )
            })?,
            generation: ResidentSandboxGeneration::new(entry.generation).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident sandbox generation is invalid",
                )
            })?,
            revision: ResidentSandboxRecordRevision::new(entry.revision)?,
            source_identity: ProjectDiskLimaSourceIdentity::parse(&entry.source_identity).map_err(
                |_| {
                    error(
                        ResidentSandboxCatalogErrorKind::InvalidInput,
                        "resident Lima source identity is invalid",
                    )
                },
            )?,
            locator: parse_locator(&entry.locator)?,
            config,
            config_digest,
            physical: decode_physical(entry.physical)?,
            last_operation_generation: entry
                .last_operation_generation
                .map(ResidentSandboxOperationGeneration::new)
                .transpose()?,
            active_operation: decode_operation(entry.active_operation)?,
        });
    }
    let catalog = ResidentSandboxCatalog {
        schema_version: wire.schema_version,
        revision,
        acceptance_requests,
        generation_maxima,
        locator_claims,
        entries,
    };
    catalog.validate()?;
    let canonical = encode_resident_sandbox_catalog(&catalog)?;
    if canonical != bytes {
        return Err(error(
            ResidentSandboxCatalogErrorKind::NonCanonical,
            "resident catalog JSON is not canonical",
        ));
    }
    Ok(catalog)
}

fn parse_locator(value: &str) -> Result<ResidentSandboxLocator, ResidentSandboxCatalogError> {
    if !valid_locator(value) {
        return Err(error(
            ResidentSandboxCatalogErrorKind::InvalidInput,
            "resident Lima locator is invalid",
        ));
    }
    Ok(ResidentSandboxLocator(value.to_owned()))
}

fn decode_physical(
    wire: ResidentSandboxPhysicalStateWire,
) -> Result<ResidentSandboxPhysicalState, ResidentSandboxCatalogError> {
    Ok(match wire {
        ResidentSandboxPhysicalStateWire::Unmaterialized => {
            ResidentSandboxPhysicalState::Unmaterialized
        }
        ResidentSandboxPhysicalStateWire::StoppedBound { host } => {
            ResidentSandboxPhysicalState::StoppedBound {
                host: decode_host(host)?,
            }
        }
        ResidentSandboxPhysicalStateWire::RunningBound { host } => {
            ResidentSandboxPhysicalState::RunningBound {
                host: decode_host(host)?,
            }
        }
        ResidentSandboxPhysicalStateWire::RevalidateRequired => {
            ResidentSandboxPhysicalState::RevalidateRequired
        }
        ResidentSandboxPhysicalStateWire::Quarantined => ResidentSandboxPhysicalState::Quarantined,
        ResidentSandboxPhysicalStateWire::RetireRequested => {
            ResidentSandboxPhysicalState::RetireRequested
        }
        ResidentSandboxPhysicalStateWire::Retired => ResidentSandboxPhysicalState::Retired,
    })
}

fn decode_host(
    wire: ResidentSandboxHostBindingWire,
) -> Result<ResidentSandboxHostBinding, ResidentSandboxCatalogError> {
    ResidentSandboxHostBinding::new(
        Sha256Digest::parse(&wire.identity_digest).map_err(|_| {
            error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident host identity digest is invalid",
            )
        })?,
        Sha256Digest::parse(&wire.config_digest).map_err(|_| {
            error(
                ResidentSandboxCatalogErrorKind::InvalidInput,
                "resident host config digest is invalid",
            )
        })?,
    )
}

fn decode_operation(
    wire: ResidentSandboxActiveOperationWire,
) -> Result<ResidentSandboxActiveOperation, ResidentSandboxCatalogError> {
    Ok(match wire {
        ResidentSandboxActiveOperationWire::None => ResidentSandboxActiveOperation::None,
        ResidentSandboxActiveOperationWire::Materialize {
            generation,
            policy_identity,
            phase,
        } => ResidentSandboxActiveOperation::Materialize {
            generation: ResidentSandboxOperationGeneration::new(generation)?,
            policy_identity: Sha256Digest::parse(&policy_identity).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident materialize policy identity is invalid",
                )
            })?,
            phase,
        },
        ResidentSandboxActiveOperationWire::Start {
            generation,
            policy_identity,
            phase,
        } => ResidentSandboxActiveOperation::Start {
            generation: ResidentSandboxOperationGeneration::new(generation)?,
            policy_identity: Sha256Digest::parse(&policy_identity).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident start policy identity is invalid",
                )
            })?,
            phase,
        },
        ResidentSandboxActiveOperationWire::Stop {
            generation,
            policy_identity,
            phase,
        } => ResidentSandboxActiveOperation::Stop {
            generation: ResidentSandboxOperationGeneration::new(generation)?,
            policy_identity: Sha256Digest::parse(&policy_identity).map_err(|_| {
                error(
                    ResidentSandboxCatalogErrorKind::InvalidInput,
                    "resident stop policy identity is invalid",
                )
            })?,
            phase,
        },
    })
}

impl From<&ResidentSandboxCatalog> for ResidentSandboxCatalogWire {
    fn from(value: &ResidentSandboxCatalog) -> Self {
        Self {
            schema_version: value.schema_version,
            revision: value.revision.get(),
            acceptance_requests: value
                .acceptance_requests
                .iter()
                .map(|request| AcceptanceRequestWire {
                    request_id: request.request_id.as_str().to_owned(),
                    project: request.project.as_str().to_owned(),
                    sandbox_id: request.sandbox_id.as_str().to_owned(),
                    source_identity: request.source_identity.digest().as_str().to_owned(),
                    locator_policy_generation: request.locator_policy_generation.get(),
                    config_digest: request.config_digest.as_str().to_owned(),
                    generation: request.generation.get(),
                    locator: request.locator.as_str().to_owned(),
                })
                .collect(),
            generation_maxima: value
                .generation_maxima
                .iter()
                .map(|maximum| GenerationMaximumWire {
                    project: maximum.lineage.project.as_str().to_owned(),
                    sandbox_id: maximum.lineage.sandbox_id.as_str().to_owned(),
                    maximum: maximum.maximum.get(),
                })
                .collect(),
            locator_claims: value
                .locator_claims
                .iter()
                .map(|claim| LocatorClaimWire {
                    source_identity: claim.source_identity.digest().as_str().to_owned(),
                    locator: claim.locator.as_str().to_owned(),
                    project: claim.key.project.as_str().to_owned(),
                    sandbox_id: claim.key.sandbox_id.as_str().to_owned(),
                    generation: claim.key.generation.get(),
                })
                .collect(),
            entries: value
                .entries
                .iter()
                .map(|entry| ResidentSandboxRecordWire {
                    project: entry.project.as_str().to_owned(),
                    sandbox_id: entry.sandbox_id.as_str().to_owned(),
                    generation: entry.generation.get(),
                    revision: entry.revision.get(),
                    source_identity: entry.source_identity.digest().as_str().to_owned(),
                    locator: entry.locator.as_str().to_owned(),
                    config: ResidentSandboxConfigWire::from(&entry.config),
                    config_digest: entry.config_digest.as_str().to_owned(),
                    physical: encode_physical(&entry.physical),
                    last_operation_generation: entry
                        .last_operation_generation
                        .map(ResidentSandboxOperationGeneration::get),
                    active_operation: encode_operation(&entry.active_operation),
                })
                .collect(),
        }
    }
}

fn encode_physical(value: &ResidentSandboxPhysicalState) -> ResidentSandboxPhysicalStateWire {
    match value {
        ResidentSandboxPhysicalState::Unmaterialized => {
            ResidentSandboxPhysicalStateWire::Unmaterialized
        }
        ResidentSandboxPhysicalState::StoppedBound { host } => {
            ResidentSandboxPhysicalStateWire::StoppedBound {
                host: ResidentSandboxHostBindingWire {
                    identity_digest: host.identity_digest.as_str().to_owned(),
                    config_digest: host.config_digest.as_str().to_owned(),
                },
            }
        }
        ResidentSandboxPhysicalState::RunningBound { host } => {
            ResidentSandboxPhysicalStateWire::RunningBound {
                host: ResidentSandboxHostBindingWire {
                    identity_digest: host.identity_digest.as_str().to_owned(),
                    config_digest: host.config_digest.as_str().to_owned(),
                },
            }
        }
        ResidentSandboxPhysicalState::RevalidateRequired => {
            ResidentSandboxPhysicalStateWire::RevalidateRequired
        }
        ResidentSandboxPhysicalState::Quarantined => ResidentSandboxPhysicalStateWire::Quarantined,
        ResidentSandboxPhysicalState::RetireRequested => {
            ResidentSandboxPhysicalStateWire::RetireRequested
        }
        ResidentSandboxPhysicalState::Retired => ResidentSandboxPhysicalStateWire::Retired,
    }
}

fn encode_operation(value: &ResidentSandboxActiveOperation) -> ResidentSandboxActiveOperationWire {
    match value {
        ResidentSandboxActiveOperation::None => ResidentSandboxActiveOperationWire::None,
        ResidentSandboxActiveOperation::Materialize {
            generation,
            policy_identity,
            phase,
        } => ResidentSandboxActiveOperationWire::Materialize {
            generation: generation.get(),
            policy_identity: policy_identity.as_str().to_owned(),
            phase: *phase,
        },
        ResidentSandboxActiveOperation::Start {
            generation,
            policy_identity,
            phase,
        } => ResidentSandboxActiveOperationWire::Start {
            generation: generation.get(),
            policy_identity: policy_identity.as_str().to_owned(),
            phase: *phase,
        },
        ResidentSandboxActiveOperation::Stop {
            generation,
            policy_identity,
            phase,
        } => ResidentSandboxActiveOperationWire::Stop {
            generation: generation.get(),
            policy_identity: policy_identity.as_str().to_owned(),
            phase: *phase,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(byte: char) -> ProjectDiskLimaSourceIdentity {
        ProjectDiskLimaSourceIdentity::parse(&format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("source identity")
    }

    fn config(
        source_identity: Option<&ProjectDiskLimaSourceIdentity>,
        generation: u64,
    ) -> ResidentSandboxConfig {
        let disk = source_identity.map(|source_identity| {
            ResidentProjectDiskConfigBinding::new(
                source_identity.clone(),
                ProjectDiskId::parse("disk-a").expect("disk id"),
                ProjectDiskGeneration::new(1).expect("disk generation"),
                LimaStandaloneDiskName::parse("smolrunner-disk-a").expect("disk locator"),
                Sha256Digest::parse(&format!("sha256:{}", "c".repeat(64)))
                    .expect("create provenance identity"),
            )
        });
        ResidentSandboxConfig::reviewed(
            ResidentPreparedTemplateGeneration::new(1).expect("template generation"),
            ResidentSandboxConfigGeneration::new(generation).expect("config generation"),
            ResidentLimaLayoutGeneration::new(1).expect("Lima layout generation"),
            ResidentResourceDeclaration::new(
                ResidentResourceGeneration::new(1).expect("resource generation"),
                2_000,
                2 * 1024 * 1024 * 1024,
                20 * 1024 * 1024 * 1024,
            )
            .expect("resources"),
            ResidentNetworkPolicyGeneration::new(1).expect("network generation"),
            ResidentCredentialPolicyGeneration::new(1).expect("credential generation"),
            ResidentGuestControlPolicyGeneration::new(1).expect("guest control generation"),
            ResidentGuestPrivilegePolicy::reviewed(
                ResidentGuestPrivilegePolicyGeneration::new(1).expect("guest privilege generation"),
            ),
            ResidentProjectIntegrationPolicyGeneration::new(1)
                .expect("project integration generation"),
            disk,
        )
        .expect("config")
    }

    fn request(
        id: &str,
        source_identity: &ProjectDiskLimaSourceIdentity,
        config_generation: u64,
    ) -> ResidentSandboxAcceptanceRequest {
        ResidentSandboxAcceptanceRequest::new(
            ResidentSandboxAcceptanceRequestId::parse(id).expect("request id"),
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").expect("project"),
            ResidentSandboxId::parse("resident-a").expect("sandbox id"),
            source_identity.clone(),
            ResidentLocatorPolicyGeneration::new(1).expect("locator policy generation"),
            config(Some(source_identity), config_generation),
        )
    }

    #[test]
    fn acceptance_allocates_monotonic_generation_and_is_idempotent() {
        let source_identity = source('a');
        let catalog = ResidentSandboxCatalog::empty();
        let (first, first_receipt) = catalog
            .accept(request("request-a", &source_identity, 1))
            .expect("first acceptance");
        assert_eq!(
            first_receipt.disposition(),
            ResidentSandboxAcceptanceDisposition::Created
        );
        assert_eq!(first_receipt.key().generation().get(), 1);
        let disk = first.entries()[0]
            .config()
            .project_disk()
            .expect("disk-bearing resident config");
        assert_eq!(disk.locator().as_str(), "smolrunner-disk-a");
        assert_eq!(
            disk.create_provenance_identity().as_str(),
            format!("sha256:{}", "c".repeat(64))
        );

        let (replayed, replay_receipt) = first
            .accept(request("request-a", &source_identity, 1))
            .expect("idempotent acceptance");
        assert_eq!(replayed, first);
        assert_eq!(
            replay_receipt.disposition(),
            ResidentSandboxAcceptanceDisposition::Satisfied
        );

        let (successor, successor_receipt) = first
            .accept(request("request-b", &source_identity, 2))
            .expect("successor acceptance");
        assert_eq!(successor_receipt.key().generation().get(), 2);
        assert_ne!(
            successor.entries()[0].locator(),
            successor.entries()[1].locator()
        );
        assert_eq!(successor.entries().len(), 2);
        assert_eq!(successor.revision().get(), 3);
    }

    #[test]
    fn request_reuse_and_source_or_config_drift_fail_closed() {
        let source_identity = source('a');
        let other_source = source('b');
        let (catalog, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        assert_eq!(
            catalog
                .accept(request("request-a", &source_identity, 2))
                .expect_err("config drift")
                .kind(),
            ResidentSandboxCatalogErrorKind::Conflict
        );
        assert_eq!(
            catalog
                .accept(request("request-a", &other_source, 1))
                .expect_err("source drift")
                .kind(),
            ResidentSandboxCatalogErrorKind::Conflict
        );
    }

    #[test]
    fn checkpoints_are_exact_no_replay_successors() {
        let source_identity = source('a');
        let (catalog, receipt) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        let authorized = catalog
            .checkpoint(
                receipt.key(),
                ResidentSandboxRecordRevision::new(1).expect("record revision"),
                ResidentSandboxCheckpoint::MaterializeAuthorized,
            )
            .expect("materialize authorization");
        assert!(matches!(
            authorized
                .find(receipt.key())
                .expect("entry")
                .active_operation(),
            ResidentSandboxActiveOperation::Materialize {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            }
        ));
        assert_eq!(
            authorized
                .find(receipt.key())
                .expect("entry")
                .last_operation_generation()
                .expect("operation generation")
                .get(),
            1
        );
        let retryable = authorized
            .checkpoint(
                receipt.key(),
                ResidentSandboxRecordRevision::new(2).expect("record revision"),
                ResidentSandboxCheckpoint::MaterializePrestartFailed,
            )
            .expect("prestart failure");
        let retry = retryable
            .checkpoint(
                receipt.key(),
                ResidentSandboxRecordRevision::new(3).expect("record revision"),
                ResidentSandboxCheckpoint::MaterializeAuthorized,
            )
            .expect("retry authorization");
        assert!(matches!(
            retry.find(receipt.key()).expect("entry").active_operation(),
            ResidentSandboxActiveOperation::Materialize {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            } if generation.get() == 2
        ));
        let started = authorized
            .checkpoint(
                receipt.key(),
                ResidentSandboxRecordRevision::new(2).expect("record revision"),
                ResidentSandboxCheckpoint::MaterializeStarted,
            )
            .expect("materialize started");
        assert!(matches!(
            started
                .find(receipt.key())
                .expect("entry")
                .active_operation(),
            ResidentSandboxActiveOperation::Materialize {
                phase: ResidentSandboxOperationPhase::Started,
                ..
            }
        ));
        assert_eq!(
            started
                .checkpoint(
                    receipt.key(),
                    ResidentSandboxRecordRevision::new(3).expect("record revision"),
                    ResidentSandboxCheckpoint::MaterializeStarted,
                )
                .expect_err("started checkpoint cannot replay")
                .kind(),
            ResidentSandboxCatalogErrorKind::InvalidSuccessor
        );
        assert!(started.validate_successor_of(&authorized).is_ok());
    }

    #[test]
    fn start_and_stop_checkpoint_vocabulary_advances_monotonic_attempts() {
        let source_identity = source('a');
        let (catalog, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        let mut stopped = catalog.entries()[0].clone();
        stopped.revision = ResidentSandboxRecordRevision::new(5).expect("record revision");
        stopped.last_operation_generation =
            Some(ResidentSandboxOperationGeneration::new(1).expect("operation generation"));
        stopped.physical = ResidentSandboxPhysicalState::StoppedBound {
            host: ResidentSandboxHostBinding::new(
                Sha256Digest::parse(&format!("sha256:{}", "d".repeat(64))).expect("host identity"),
                stopped.config_digest.clone(),
            )
            .expect("host binding"),
        };
        validate_record(&stopped).expect("stopped record");

        let start_authorized = stopped.begin_start().expect("start authorized");
        assert!(matches!(
            start_authorized.active_operation(),
            ResidentSandboxActiveOperation::Start {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            } if generation.get() == 2
        ));
        let start_retryable = start_authorized
            .checkpoint_start_prestart_failed()
            .expect("start prestart failure");
        assert!(matches!(
            start_retryable.active_operation(),
            ResidentSandboxActiveOperation::None
        ));
        let start_started = start_authorized
            .checkpoint_start_started()
            .expect("start started");
        let start_recovery = start_started
            .checkpoint_start_recovery_required()
            .expect("start recovery");
        assert!(valid_operation_successor(&stopped, &start_authorized));
        assert!(valid_operation_successor(&start_authorized, &start_started));
        assert!(valid_operation_successor(&start_started, &start_recovery));
        assert!(start_started.checkpoint_start_started().is_err());

        let mut running = stopped;
        running.revision = ResidentSandboxRecordRevision::new(9).expect("record revision");
        running.last_operation_generation =
            Some(ResidentSandboxOperationGeneration::new(4).expect("operation generation"));
        running.physical = ResidentSandboxPhysicalState::RunningBound {
            host: match running.physical {
                ResidentSandboxPhysicalState::StoppedBound { host } => host,
                _ => unreachable!("test record is stopped-bound"),
            },
        };
        let stop_authorized = running.begin_stop().expect("stop authorized");
        assert!(matches!(
            stop_authorized.active_operation(),
            ResidentSandboxActiveOperation::Stop {
                generation,
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            } if generation.get() == 5
        ));
        assert!(stop_authorized.checkpoint_stop_prestart_failed().is_ok());
        let stop_started = stop_authorized
            .checkpoint_stop_started()
            .expect("stop started");
        assert!(stop_started.checkpoint_stop_recovery_required().is_ok());
        assert!(stop_started.checkpoint_stop_started().is_err());
    }

    #[test]
    fn acceptance_successor_cannot_rewrite_an_existing_idempotency_claim() {
        let source_identity = source('a');
        let (current, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("first acceptance");
        let (mutated, _) = current
            .accept(request("request-b", &source_identity, 2))
            .expect("second acceptance");
        let mut mutated = mutated;
        mutated.acceptance_requests[0].request_id =
            ResidentSandboxAcceptanceRequestId::parse("request-aa").expect("replacement request");
        assert!(mutated.validate().is_ok());
        assert_eq!(
            mutated
                .validate_successor_of(&current)
                .expect_err("permanent request claim was rewritten")
                .kind(),
            ResidentSandboxCatalogErrorKind::InvalidSuccessor
        );
    }

    #[test]
    fn decoded_claims_cannot_inject_a_locator_or_generation_gap() {
        let source_identity = source('a');
        let (catalog, receipt) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");

        let mut locator = catalog.clone();
        let replacement = ResidentSandboxLocator("smolrunner-forged".to_owned());
        locator.entries[0].locator = replacement.clone();
        locator.acceptance_requests[0].locator = replacement.clone();
        locator.locator_claims[0].locator = replacement;
        assert_eq!(
            locator
                .validate()
                .expect_err("locator must remain controller-derived")
                .kind(),
            ResidentSandboxCatalogErrorKind::CorruptState
        );

        let mut generation_gap = catalog.clone();
        generation_gap.generation_maxima[0].maximum =
            ResidentSandboxGeneration::new(2).expect("generation");
        assert_eq!(
            generation_gap
                .validate()
                .expect_err("generation maximum must match retained history")
                .kind(),
            ResidentSandboxCatalogErrorKind::CorruptState
        );

        let authorized = catalog
            .checkpoint(
                receipt.key(),
                ResidentSandboxRecordRevision::new(1).expect("record revision"),
                ResidentSandboxCheckpoint::MaterializeAuthorized,
            )
            .expect("materialize authorization");
        let mut value: serde_json::Value = serde_json::from_slice(
            &encode_resident_sandbox_catalog(&authorized).expect("encode authorized"),
        )
        .expect("value");
        value["entries"][0]["active_operation"]["policy_identity"] =
            serde_json::Value::String(format!("sha256:{}", "e".repeat(64)));
        assert_eq!(
            decode_resident_sandbox_catalog(&serde_json::to_vec(&value).expect("forged policy"))
                .expect_err("operation policy identity is controller-derived")
                .kind(),
            ResidentSandboxCatalogErrorKind::CorruptState
        );
    }

    #[test]
    fn reviewed_guest_policy_has_no_caller_selected_supplementary_groups() {
        let policy = ResidentGuestPrivilegePolicy::reviewed(
            ResidentGuestPrivilegePolicyGeneration::new(1).expect("policy generation"),
        );
        assert!(policy.task_supplementary_groups().is_empty());

        let source_identity = source('a');
        let (catalog, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        let mut value: serde_json::Value =
            serde_json::from_slice(&encode_resident_sandbox_catalog(&catalog).expect("encode"))
                .expect("value");
        value["entries"][0]["config"]["guest_privilege_policy"]["task_supplementary_groups"] =
            serde_json::json!(["wheel"]);
        assert_eq!(
            decode_resident_sandbox_catalog(&serde_json::to_vec(&value).expect("mutated JSON"))
                .expect_err("unreviewed supplementary group")
                .kind(),
            ResidentSandboxCatalogErrorKind::CorruptState
        );
    }

    #[test]
    fn canonical_codec_round_trips_and_rejects_unknown_duplicate_or_noncanonical_json() {
        let source_identity = source('a');
        let (catalog, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        let bytes = encode_resident_sandbox_catalog(&catalog).expect("encode");
        assert_eq!(
            decode_resident_sandbox_catalog(&bytes).expect("round trip"),
            catalog
        );
        assert!(matches!(
            decode_resident_sandbox_catalog(
                &serde_json::to_vec_pretty(
                    &serde_json::from_slice::<serde_json::Value>(&bytes).expect("value")
                )
                .expect("pretty JSON")
            )
            .expect_err("pretty JSON is noncanonical")
            .kind(),
            ResidentSandboxCatalogErrorKind::NonCanonical
        ));

        let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).expect("value");
        unknown["unexpected"] = serde_json::Value::Bool(true);
        assert_eq!(
            decode_resident_sandbox_catalog(&serde_json::to_vec(&unknown).expect("unknown"))
                .expect_err("unknown field")
                .kind(),
            ResidentSandboxCatalogErrorKind::CorruptState
        );

        let duplicate = br#"{"schema_version":1,"schema_version":1,"revision":1,"acceptance_requests":[],"generation_maxima":[],"locator_claims":[],"entries":[]}"#;
        assert_eq!(
            decode_resident_sandbox_catalog(duplicate)
                .expect_err("duplicate field")
                .kind(),
            ResidentSandboxCatalogErrorKind::CorruptState
        );
        assert_eq!(
            decode_resident_sandbox_catalog(&vec![
                b'x';
                MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES + 1
            ])
            .expect_err("oversized catalog")
            .kind(),
            ResidentSandboxCatalogErrorKind::LimitExceeded
        );
    }

    #[test]
    fn locator_collision_and_project_disk_source_mismatch_are_rejected() {
        let source_identity = source('a');
        let (catalog, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        let mismatched_disk = ResidentProjectDiskConfigBinding::new(
            source('b'),
            ProjectDiskId::parse("disk-a").expect("disk id"),
            ProjectDiskGeneration::new(1).expect("disk generation"),
            LimaStandaloneDiskName::parse("smolrunner-disk-a").expect("disk locator"),
            Sha256Digest::parse(&format!("sha256:{}", "c".repeat(64)))
                .expect("create provenance identity"),
        );
        let mismatched_config = ResidentSandboxConfig::reviewed(
            ResidentPreparedTemplateGeneration::new(1).expect("generation"),
            ResidentSandboxConfigGeneration::new(1).expect("generation"),
            ResidentLimaLayoutGeneration::new(1).expect("generation"),
            ResidentResourceDeclaration::new(
                ResidentResourceGeneration::new(1).expect("generation"),
                2_000,
                2 * 1024 * 1024 * 1024,
                20 * 1024 * 1024 * 1024,
            )
            .expect("resources"),
            ResidentNetworkPolicyGeneration::new(1).expect("generation"),
            ResidentCredentialPolicyGeneration::new(1).expect("generation"),
            ResidentGuestControlPolicyGeneration::new(1).expect("generation"),
            ResidentGuestPrivilegePolicy::reviewed(
                ResidentGuestPrivilegePolicyGeneration::new(1).expect("generation"),
            ),
            ResidentProjectIntegrationPolicyGeneration::new(1).expect("generation"),
            Some(mismatched_disk),
        )
        .expect("declaration construction");
        let mismatched_request = ResidentSandboxAcceptanceRequest::new(
            ResidentSandboxAcceptanceRequestId::parse("request-mismatch").expect("request id"),
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").expect("project"),
            ResidentSandboxId::parse("resident-a").expect("sandbox id"),
            source_identity.clone(),
            ResidentLocatorPolicyGeneration::new(1).expect("locator policy generation"),
            mismatched_config,
        );
        assert_eq!(
            catalog
                .accept(mismatched_request)
                .expect_err("cross-source project disk binding")
                .kind(),
            ResidentSandboxCatalogErrorKind::InvalidInput
        );

        let mut value: serde_json::Value =
            serde_json::from_slice(&encode_resident_sandbox_catalog(&catalog).expect("encode"))
                .expect("value");
        let locator = value["entries"][0]["locator"].clone();
        let (two, _) = catalog
            .accept(request("request-b", &source_identity, 2))
            .expect("second acceptance");
        value = serde_json::from_slice(&encode_resident_sandbox_catalog(&two).expect("encode two"))
            .expect("value two");
        value["entries"][1]["locator"] = locator.clone();
        value["locator_claims"][1]["locator"] = locator;
        assert_eq!(
            decode_resident_sandbox_catalog(&serde_json::to_vec(&value).expect("collision"))
                .expect_err("collision")
                .kind(),
            ResidentSandboxCatalogErrorKind::Duplicate
        );
    }

    #[test]
    fn codec_does_not_expose_private_paths_or_runtime_capabilities() {
        let source_identity = source('a');
        let (catalog, _) = ResidentSandboxCatalog::empty()
            .accept(request("request-a", &source_identity, 1))
            .expect("acceptance");
        let bytes = encode_resident_sandbox_catalog(&catalog).expect("encode");
        let text = String::from_utf8(bytes).expect("UTF-8");
        assert!(!text.contains("/private/"));
        assert!(!text.contains("LIMA_HOME"));
        assert!(!text.contains("/dev/fd"));
        assert!(!text.contains("\"fd\":"));
    }
}
