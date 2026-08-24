//! Pure durable identity and no-replay planning for one Tier-3 resident sandbox generation.
//!
//! This module owns the logical `ResidentSandboxId + ResidentSandboxGeneration` namespace before
//! project-disk attachment or guest-control code may treat that pair as authority. It derives the
//! Lima instance locator from immutable accepted inputs, binds one exact reviewed Lima observation
//! request, and models the pre-materialization no-replay checkpoints. It performs no persistence,
//! host observation, Lima execution, VM start/stop, disk mutation, guest invocation, or proof minting.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationRequest, LimaObservationSourceIdentity,
    LimaVmType,
};
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ResidentSandboxGeneration, ResidentSandboxId};

pub const RESIDENT_SANDBOX_GENERATION_SCHEMA_VERSION: u8 = 1;
const LIMA_LOCATOR_DOMAIN: &[u8] = b"smolrunner-resident-sandbox-lima-locator-v1";
const LIMA_LOCATOR_PREFIX: &str = "smolr-res-";
const LIMA_LOCATOR_HEX_BYTES: usize = 32;
const REDACTED_LIMA_SOURCE: &str = "<private-lima-source>";

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Construct one positive generation.
            ///
            /// # Errors
            ///
            /// Returns a bounded error when `value` is zero.
            pub fn new(value: u64) -> Result<Self, ResidentSandboxGenerationError> {
                if value == 0 {
                    return Err(error($code, $message));
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

positive_generation_type!(
    ResidentSandboxRevision,
    "resident_sandbox_revision_invalid",
    "resident sandbox revision must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxPreparedGeneration,
    "resident_sandbox_prepared_generation_invalid",
    "resident sandbox prepared generation must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxConfigGeneration,
    "resident_sandbox_config_generation_invalid",
    "resident sandbox config generation must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxResourceGeneration,
    "resident_sandbox_resource_generation_invalid",
    "resident sandbox resource generation must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxCredentialGeneration,
    "resident_sandbox_credential_generation_invalid",
    "resident sandbox credential generation must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxNetworkGeneration,
    "resident_sandbox_network_generation_invalid",
    "resident sandbox network generation must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxMaterializeAttemptGeneration,
    "resident_sandbox_materialize_attempt_generation_invalid",
    "resident sandbox materialize attempt generation must be greater than zero"
);

impl ResidentSandboxRevision {
    fn next(self) -> Result<Self, ResidentSandboxGenerationError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

impl ResidentSandboxMaterializeAttemptGeneration {
    fn next(self) -> Result<Self, ResidentSandboxGenerationError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

/// Immutable accepted declaration used to derive one resident-sandbox Lima locator.
///
/// Callers choose logical generations and policy digests. They cannot choose or override the Lima
/// instance name: it is derived canonically from this declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxGenerationIntent {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    prepared_generation: ResidentSandboxPreparedGeneration,
    config_generation: ResidentSandboxConfigGeneration,
    config_digest: Sha256Digest,
    resource_generation: ResidentSandboxResourceGeneration,
    credential_generation: ResidentSandboxCredentialGeneration,
    network_generation: ResidentSandboxNetworkGeneration,
    lima_locator: LimaInstanceName,
}

impl ResidentSandboxGenerationIntent {
    /// Build one immutable resident-sandbox generation declaration and derive its Lima locator.
    #[must_use]
    pub fn new(
        project: ProjectIdentity,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        prepared_generation: ResidentSandboxPreparedGeneration,
        config_generation: ResidentSandboxConfigGeneration,
        config_digest: Sha256Digest,
        resource_generation: ResidentSandboxResourceGeneration,
        credential_generation: ResidentSandboxCredentialGeneration,
        network_generation: ResidentSandboxNetworkGeneration,
    ) -> Self {
        let lima_locator = derive_lima_locator(
            &project,
            &sandbox_id,
            sandbox_generation,
            prepared_generation,
            config_generation,
            &config_digest,
            resource_generation,
            credential_generation,
            network_generation,
        );
        Self {
            project,
            sandbox_id,
            sandbox_generation,
            prepared_generation,
            config_generation,
            config_digest,
            resource_generation,
            credential_generation,
            network_generation,
            lima_locator,
        }
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
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn lima_locator(&self) -> &LimaInstanceName {
        &self.lima_locator
    }

    /// Accept this declaration only against the exact derived VZ/aarch64 Lima request.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal if the request targets another instance, Lima source/configuration,
    /// backend, or architecture.
    pub fn accept(
        self,
        request: &LimaObservationRequest,
    ) -> Result<ResidentSandboxGenerationRecord, ResidentSandboxGenerationError> {
        require_request_contract(&self.lima_locator, request)?;
        Ok(ResidentSandboxGenerationRecord {
            schema_version: RESIDENT_SANDBOX_GENERATION_SCHEMA_VERSION,
            project: self.project,
            sandbox_id: self.sandbox_id,
            sandbox_generation: self.sandbox_generation,
            revision: ResidentSandboxRevision(1),
            prepared_generation: self.prepared_generation,
            config_generation: self.config_generation,
            config_digest: self.config_digest,
            resource_generation: self.resource_generation,
            credential_generation: self.credential_generation,
            network_generation: self.network_generation,
            lima_locator: self.lima_locator,
            lima_request_digest: request.request_identity().digest().clone(),
            last_materialize_attempt: None,
            state: ResidentSandboxGenerationState::Accepted,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResidentSandboxGenerationState {
    Accepted,
    MaterializeAuthorized {
        attempt_generation: ResidentSandboxMaterializeAttemptGeneration,
    },
    MaterializeStarted {
        attempt_generation: ResidentSandboxMaterializeAttemptGeneration,
    },
    RevalidateRequired {
        #[serde(skip_serializing_if = "Option::is_none")]
        attempt_generation: Option<ResidentSandboxMaterializeAttemptGeneration>,
    },
    Quarantined,
}

/// Durable pure record for one logical resident-sandbox generation.
///
/// The record intentionally carries only declaration/checkpoint identity. Physical Lima/VZ binding
/// arrives in a later #653 observer slice after durable pre-mutation ownership exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxGenerationRecord {
    schema_version: u8,
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    revision: ResidentSandboxRevision,
    prepared_generation: ResidentSandboxPreparedGeneration,
    config_generation: ResidentSandboxConfigGeneration,
    config_digest: Sha256Digest,
    resource_generation: ResidentSandboxResourceGeneration,
    credential_generation: ResidentSandboxCredentialGeneration,
    network_generation: ResidentSandboxNetworkGeneration,
    lima_locator: LimaInstanceName,
    lima_request_digest: Sha256Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_materialize_attempt: Option<ResidentSandboxMaterializeAttemptGeneration>,
    state: ResidentSandboxGenerationState,
}

impl ResidentSandboxGenerationRecord {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
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
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn revision(&self) -> ResidentSandboxRevision {
        self.revision
    }

    #[must_use]
    pub const fn prepared_generation(&self) -> ResidentSandboxPreparedGeneration {
        self.prepared_generation
    }

    #[must_use]
    pub const fn config_generation(&self) -> ResidentSandboxConfigGeneration {
        self.config_generation
    }

    #[must_use]
    pub const fn config_digest(&self) -> &Sha256Digest {
        &self.config_digest
    }

    #[must_use]
    pub const fn resource_generation(&self) -> ResidentSandboxResourceGeneration {
        self.resource_generation
    }

    #[must_use]
    pub const fn credential_generation(&self) -> ResidentSandboxCredentialGeneration {
        self.credential_generation
    }

    #[must_use]
    pub const fn network_generation(&self) -> ResidentSandboxNetworkGeneration {
        self.network_generation
    }

    #[must_use]
    pub const fn lima_locator(&self) -> &LimaInstanceName {
        &self.lima_locator
    }

    #[must_use]
    pub const fn lima_request_digest(&self) -> &Sha256Digest {
        &self.lima_request_digest
    }

    #[must_use]
    pub const fn last_materialize_attempt(
        &self,
    ) -> Option<ResidentSandboxMaterializeAttemptGeneration> {
        self.last_materialize_attempt
    }

    #[must_use]
    pub const fn state(&self) -> &ResidentSandboxGenerationState {
        &self.state
    }

    /// Bind this exact record revision to the exact previously accepted private Lima source/request.
    ///
    /// This is declaration correlation only. It proves no VM existence, running state, host identity,
    /// or physical ownership.
    pub fn bind_lima_request(
        &self,
        request: &LimaObservationRequest,
    ) -> Result<ResidentSandboxLimaBinding, ResidentSandboxGenerationError> {
        self.require_bindable_state()?;
        self.confirm_request(request)?;
        Ok(ResidentSandboxLimaBinding {
            project: self.project.clone(),
            sandbox_id: self.sandbox_id.clone(),
            sandbox_generation: self.sandbox_generation,
            record_revision: self.revision,
            lima_locator: self.lima_locator.clone(),
            lima_request_digest: self.lima_request_digest.clone(),
            source_identity: request.source_identity(),
        })
    }

    /// Plan the next external materialization attempt from accepted state only.
    pub fn plan_materialize(
        &self,
    ) -> Result<ResidentSandboxMaterializePlan, ResidentSandboxGenerationError> {
        if !matches!(self.state, ResidentSandboxGenerationState::Accepted) {
            return Err(invalid_state(
                "resident_sandbox_materialize_requires_accepted",
                "resident sandbox materialization requires accepted state",
            ));
        }
        let attempt_generation = self.next_materialize_attempt()?;
        Ok(ResidentSandboxMaterializePlan {
            identity: self.plan_identity(),
            attempt_generation,
            lima_locator: self.lima_locator.clone(),
            lima_request_digest: self.lima_request_digest.clone(),
        })
    }

    /// Record the durable pre-execution authorization for one exact materialization plan.
    pub fn record_materialize_authorized(
        &self,
        plan: &ResidentSandboxMaterializePlan,
    ) -> Result<Self, ResidentSandboxGenerationError> {
        if !matches!(self.state, ResidentSandboxGenerationState::Accepted) {
            return Err(invalid_state(
                "resident_sandbox_materialize_requires_accepted",
                "resident sandbox materialization authorization requires accepted state",
            ));
        }
        self.confirm_plan(plan)?;
        self.successor(
            ResidentSandboxGenerationState::MaterializeAuthorized {
                attempt_generation: plan.attempt_generation,
            },
            Some(plan.attempt_generation),
        )
    }

    /// Return one exact start plan only after materialization authorization is durably current.
    pub fn plan_materialize_start(
        &self,
    ) -> Result<ResidentSandboxMaterializeStartPlan, ResidentSandboxGenerationError> {
        let ResidentSandboxGenerationState::MaterializeAuthorized { attempt_generation } = self.state
        else {
            return Err(invalid_state(
                "resident_sandbox_materialize_authorization_required",
                "resident sandbox materialization start requires authorized state",
            ));
        };
        Ok(ResidentSandboxMaterializeStartPlan {
            identity: self.plan_identity(),
            attempt_generation,
            lima_locator: self.lima_locator.clone(),
            lima_request_digest: self.lima_request_digest.clone(),
        })
    }

    /// Persist the no-replay `MaterializeStarted` checkpoint before external create/start execution.
    pub fn record_materialize_started(
        &self,
        start: &ResidentSandboxMaterializeStartPlan,
    ) -> Result<Self, ResidentSandboxGenerationError> {
        let ResidentSandboxGenerationState::MaterializeAuthorized { attempt_generation } = self.state
        else {
            return Err(invalid_state(
                "resident_sandbox_materialize_authorization_required",
                "resident sandbox materialization start requires authorized state",
            ));
        };
        self.confirm_plan_identity(&start.identity)?;
        if attempt_generation != start.attempt_generation
            || start.lima_locator != self.lima_locator
            || start.lima_request_digest != self.lima_request_digest
        {
            return Err(plan_mismatch());
        }
        self.successor(
            ResidentSandboxGenerationState::MaterializeStarted { attempt_generation },
            self.last_materialize_attempt,
        )
    }

    /// Return to accepted state only before the durable started checkpoint exists.
    pub fn record_prestart_failure(
        &self,
        plan: &ResidentSandboxMaterializePlan,
    ) -> Result<Self, ResidentSandboxGenerationError> {
        let ResidentSandboxGenerationState::MaterializeAuthorized { attempt_generation } = self.state
        else {
            return Err(invalid_state(
                "resident_sandbox_materialize_authorization_required",
                "resident sandbox pre-start failure requires authorized state",
            ));
        };
        self.confirm_plan(plan)?;
        if attempt_generation != plan.attempt_generation {
            return Err(plan_mismatch());
        }
        self.successor(
            ResidentSandboxGenerationState::Accepted,
            self.last_materialize_attempt,
        )
    }

    /// Remove optimistic continuation authority after a policy/physical uncertainty boundary.
    pub fn require_revalidation(&self) -> Result<Self, ResidentSandboxGenerationError> {
        let attempt_generation = match self.state {
            ResidentSandboxGenerationState::Accepted => None,
            ResidentSandboxGenerationState::MaterializeStarted { attempt_generation } => {
                Some(attempt_generation)
            }
            ResidentSandboxGenerationState::RevalidateRequired { .. } => return Ok(self.clone()),
            _ => {
                return Err(invalid_state(
                    "resident_sandbox_revalidation_transition_invalid",
                    "resident sandbox state cannot enter revalidation",
                ));
            }
        };
        self.successor(
            ResidentSandboxGenerationState::RevalidateRequired { attempt_generation },
            self.last_materialize_attempt,
        )
    }

    /// Quarantine this generation. Quarantine removes authority and performs no external action.
    pub fn quarantine(&self) -> Result<Self, ResidentSandboxGenerationError> {
        if matches!(self.state, ResidentSandboxGenerationState::Quarantined) {
            return Ok(self.clone());
        }
        self.successor(
            ResidentSandboxGenerationState::Quarantined,
            self.last_materialize_attempt,
        )
    }

    fn confirm_request(
        &self,
        request: &LimaObservationRequest,
    ) -> Result<(), ResidentSandboxGenerationError> {
        require_request_contract(&self.lima_locator, request)?;
        if request.request_identity().digest() != &self.lima_request_digest {
            return Err(error(
                "resident_sandbox_lima_request_mismatch",
                "resident sandbox Lima request identity does not match the accepted generation",
            ));
        }
        Ok(())
    }

    fn require_bindable_state(&self) -> Result<(), ResidentSandboxGenerationError> {
        if matches!(self.state, ResidentSandboxGenerationState::Quarantined) {
            return Err(invalid_state(
                "resident_sandbox_quarantined",
                "quarantined resident sandbox generation carries no Lima binding authority",
            ));
        }
        Ok(())
    }

    fn next_materialize_attempt(
        &self,
    ) -> Result<ResidentSandboxMaterializeAttemptGeneration, ResidentSandboxGenerationError> {
        match self.last_materialize_attempt {
            Some(generation) => generation.next(),
            None => ResidentSandboxMaterializeAttemptGeneration::new(1),
        }
    }

    fn plan_identity(&self) -> ResidentSandboxPlanIdentity {
        ResidentSandboxPlanIdentity {
            project: self.project.clone(),
            sandbox_id: self.sandbox_id.clone(),
            sandbox_generation: self.sandbox_generation,
            expected_revision: self.revision,
        }
    }

    fn confirm_plan(
        &self,
        plan: &ResidentSandboxMaterializePlan,
    ) -> Result<(), ResidentSandboxGenerationError> {
        self.confirm_plan_identity(&plan.identity)?;
        if plan.attempt_generation != self.next_materialize_attempt()?
            || plan.lima_locator != self.lima_locator
            || plan.lima_request_digest != self.lima_request_digest
        {
            return Err(plan_mismatch());
        }
        Ok(())
    }

    fn confirm_plan_identity(
        &self,
        identity: &ResidentSandboxPlanIdentity,
    ) -> Result<(), ResidentSandboxGenerationError> {
        if identity.project != self.project
            || identity.sandbox_id != self.sandbox_id
            || identity.sandbox_generation != self.sandbox_generation
        {
            return Err(plan_mismatch());
        }
        if identity.expected_revision != self.revision {
            return Err(error(
                "resident_sandbox_plan_stale",
                "resident sandbox plan revision is stale",
            ));
        }
        Ok(())
    }

    fn successor(
        &self,
        state: ResidentSandboxGenerationState,
        last_materialize_attempt: Option<ResidentSandboxMaterializeAttemptGeneration>,
    ) -> Result<Self, ResidentSandboxGenerationError> {
        Ok(Self {
            schema_version: self.schema_version,
            project: self.project.clone(),
            sandbox_id: self.sandbox_id.clone(),
            sandbox_generation: self.sandbox_generation,
            revision: self.revision.next()?,
            prepared_generation: self.prepared_generation,
            config_generation: self.config_generation,
            config_digest: self.config_digest.clone(),
            resource_generation: self.resource_generation,
            credential_generation: self.credential_generation,
            network_generation: self.network_generation,
            lima_locator: self.lima_locator.clone(),
            lima_request_digest: self.lima_request_digest.clone(),
            last_materialize_attempt,
            state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxPlanIdentity {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    expected_revision: ResidentSandboxRevision,
}

impl ResidentSandboxPlanIdentity {
    #[must_use]
    pub const fn expected_revision(&self) -> ResidentSandboxRevision {
        self.expected_revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxMaterializePlan {
    identity: ResidentSandboxPlanIdentity,
    attempt_generation: ResidentSandboxMaterializeAttemptGeneration,
    lima_locator: LimaInstanceName,
    lima_request_digest: Sha256Digest,
}

impl ResidentSandboxMaterializePlan {
    #[must_use]
    pub const fn attempt_generation(&self) -> ResidentSandboxMaterializeAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn lima_locator(&self) -> &LimaInstanceName {
        &self.lima_locator
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxMaterializeStartPlan {
    identity: ResidentSandboxPlanIdentity,
    attempt_generation: ResidentSandboxMaterializeAttemptGeneration,
    lima_locator: LimaInstanceName,
    lima_request_digest: Sha256Digest,
}

impl ResidentSandboxMaterializeStartPlan {
    #[must_use]
    pub const fn attempt_generation(&self) -> ResidentSandboxMaterializeAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn lima_locator(&self) -> &LimaInstanceName {
        &self.lima_locator
    }
}

/// Short-lived declaration binding between one exact resident-sandbox record revision and Lima source.
///
/// This value is deliberately non-serializable and non-cloneable. It proves namespace/configuration
/// equality only; a later descriptor-bound observer must add current physical host identity.
pub struct ResidentSandboxLimaBinding {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    record_revision: ResidentSandboxRevision,
    lima_locator: LimaInstanceName,
    lima_request_digest: Sha256Digest,
    source_identity: LimaObservationSourceIdentity,
}

impl ResidentSandboxLimaBinding {
    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn record_revision(&self) -> ResidentSandboxRevision {
        self.record_revision
    }

    #[must_use]
    pub const fn lima_locator(&self) -> &LimaInstanceName {
        &self.lima_locator
    }

    /// Reconfirm this binding against the exact current record revision and Lima request.
    pub fn confirm(
        &self,
        record: &ResidentSandboxGenerationRecord,
        request: &LimaObservationRequest,
    ) -> Result<(), ResidentSandboxGenerationError> {
        if self.project != record.project
            || self.sandbox_id != record.sandbox_id
            || self.sandbox_generation != record.sandbox_generation
            || self.record_revision != record.revision
            || self.lima_locator != record.lima_locator
            || self.lima_request_digest != record.lima_request_digest
        {
            return Err(error(
                "resident_sandbox_binding_stale",
                "resident sandbox Lima binding does not match the current record revision",
            ));
        }
        record.confirm_request(request)?;
        if self.source_identity != request.source_identity() {
            return Err(error(
                "resident_sandbox_lima_source_mismatch",
                "resident sandbox Lima source identity does not match the accepted binding",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ResidentSandboxLimaBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentSandboxLimaBinding")
            .field("project", &self.project)
            .field("sandbox_id", &self.sandbox_id)
            .field("sandbox_generation", &self.sandbox_generation)
            .field("record_revision", &self.record_revision)
            .field("lima_locator", &self.lima_locator)
            .field("lima_request_digest", &self.lima_request_digest)
            .field("source_identity", &REDACTED_LIMA_SOURCE)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxGenerationError {
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for ResidentSandboxGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentSandboxGenerationError")
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ResidentSandboxGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ResidentSandboxGenerationError {}

fn require_request_contract(
    expected_locator: &LimaInstanceName,
    request: &LimaObservationRequest,
) -> Result<(), ResidentSandboxGenerationError> {
    if request.instance() != expected_locator {
        return Err(error(
            "resident_sandbox_lima_locator_mismatch",
            "resident sandbox Lima request must use the controller-derived instance locator",
        ));
    }
    if request.expected_vm_type() != LimaVmType::Vz {
        return Err(error(
            "resident_sandbox_vm_type_mismatch",
            "resident sandbox Lima request must use the reviewed VZ backend",
        ));
    }
    if request.expected_architecture() != LimaArchitecture::Aarch64 {
        return Err(error(
            "resident_sandbox_architecture_mismatch",
            "resident sandbox Lima request must use the reviewed aarch64 architecture",
        ));
    }
    Ok(())
}

fn derive_lima_locator(
    project: &ProjectIdentity,
    sandbox_id: &ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    prepared_generation: ResidentSandboxPreparedGeneration,
    config_generation: ResidentSandboxConfigGeneration,
    config_digest: &Sha256Digest,
    resource_generation: ResidentSandboxResourceGeneration,
    credential_generation: ResidentSandboxCredentialGeneration,
    network_generation: ResidentSandboxNetworkGeneration,
) -> LimaInstanceName {
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, LIMA_LOCATOR_DOMAIN);
    hash_part(&mut hasher, project.as_str().as_bytes());
    hash_part(&mut hasher, sandbox_id.as_str().as_bytes());
    hash_part(&mut hasher, &sandbox_generation.get().to_be_bytes());
    hash_part(&mut hasher, &prepared_generation.get().to_be_bytes());
    hash_part(&mut hasher, &config_generation.get().to_be_bytes());
    hash_part(&mut hasher, config_digest.as_str().as_bytes());
    hash_part(&mut hasher, &resource_generation.get().to_be_bytes());
    hash_part(&mut hasher, &credential_generation.get().to_be_bytes());
    hash_part(&mut hasher, &network_generation.get().to_be_bytes());
    let hex = format!("{:x}", hasher.finalize());
    let locator = format!("{LIMA_LOCATOR_PREFIX}{}", &hex[..LIMA_LOCATOR_HEX_BYTES]);
    LimaInstanceName::parse(&locator).expect("derived resident sandbox locator is canonical")
}

fn hash_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

const fn error(code: &'static str, message: &'static str) -> ResidentSandboxGenerationError {
    ResidentSandboxGenerationError { code, message }
}

const fn invalid_state(
    code: &'static str,
    message: &'static str,
) -> ResidentSandboxGenerationError {
    error(code, message)
}

const fn plan_mismatch() -> ResidentSandboxGenerationError {
    error(
        "resident_sandbox_plan_mismatch",
        "resident sandbox materialization plan does not match the current generation",
    )
}

const fn generation_exhausted() -> ResidentSandboxGenerationError {
    error(
        "resident_sandbox_generation_exhausted",
        "resident sandbox generation counter is exhausted",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ResidentSandboxConfigGeneration, ResidentSandboxCredentialGeneration,
        ResidentSandboxGenerationIntent, ResidentSandboxGenerationState,
        ResidentSandboxNetworkGeneration, ResidentSandboxPreparedGeneration,
        ResidentSandboxResourceGeneration,
    };
    use crate::artifact::Sha256Digest;
    use crate::lima_observation::{
        LimaArchitecture, LimaInstanceName, LimaObservationRequest, LimaVmType,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ResidentSandboxGeneration, ResidentSandboxId};

    const CONFIG_DIGEST: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn intent(sandbox: &str, generation: u64) -> ResidentSandboxGenerationIntent {
        ResidentSandboxGenerationIntent::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ResidentSandboxId::parse(sandbox).unwrap(),
            ResidentSandboxGeneration::new(generation).unwrap(),
            ResidentSandboxPreparedGeneration::new(3).unwrap(),
            ResidentSandboxConfigGeneration::new(5).unwrap(),
            Sha256Digest::parse(CONFIG_DIGEST).unwrap(),
            ResidentSandboxResourceGeneration::new(2).unwrap(),
            ResidentSandboxCredentialGeneration::new(4).unwrap(),
            ResidentSandboxNetworkGeneration::new(6).unwrap(),
        )
    }

    fn request(locator: LimaInstanceName, home: &str) -> LimaObservationRequest {
        LimaObservationRequest::new(
            locator,
            home,
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            "/var/cache/smolrunner",
            30,
        )
        .unwrap()
    }

    #[test]
    fn locator_is_derived_and_caller_selected_instance_is_rejected() {
        let intent = intent("sandbox-a", 7);
        assert!(intent.lima_locator().as_str().starts_with("smolr-res-"));
        let wrong = request(LimaInstanceName::parse("caller-chosen").unwrap(), "/tmp/lima-a");
        assert_eq!(
            intent.clone().accept(&wrong).unwrap_err().code,
            "resident_sandbox_lima_locator_mismatch"
        );

        let accepted = intent
            .clone()
            .accept(&request(intent.lima_locator().clone(), "/tmp/lima-a"))
            .unwrap();
        assert_eq!(accepted.sandbox_generation().get(), 7);
        assert_eq!(accepted.lima_locator(), intent.lima_locator());
    }

    #[test]
    fn lima_binding_is_exact_to_record_revision_and_private_source() {
        let intent = intent("sandbox-a", 7);
        let exact_request = request(intent.lima_locator().clone(), "/tmp/lima-a");
        let accepted = intent.accept(&exact_request).unwrap();
        let binding = accepted.bind_lima_request(&exact_request).unwrap();
        binding.confirm(&accepted, &exact_request).unwrap();

        let materialize = accepted.plan_materialize().unwrap();
        let authorized = accepted.record_materialize_authorized(&materialize).unwrap();
        assert_eq!(
            binding.confirm(&authorized, &exact_request).unwrap_err().code,
            "resident_sandbox_binding_stale"
        );
        let fresh = authorized.bind_lima_request(&exact_request).unwrap();
        fresh.confirm(&authorized, &exact_request).unwrap();

        let other_source = request(authorized.lima_locator().clone(), "/tmp/lima-b");
        assert_eq!(
            authorized.bind_lima_request(&other_source).unwrap_err().code,
            "resident_sandbox_lima_request_mismatch"
        );
    }

    #[test]
    fn materialize_started_is_a_no_replay_boundary() {
        let intent = intent("sandbox-a", 7);
        let exact_request = request(intent.lima_locator().clone(), "/tmp/lima-a");
        let accepted = intent.accept(&exact_request).unwrap();
        let plan = accepted.plan_materialize().unwrap();
        assert_eq!(plan.attempt_generation().get(), 1);
        let authorized = accepted.record_materialize_authorized(&plan).unwrap();
        let start = authorized.plan_materialize_start().unwrap();
        let started = authorized.record_materialize_started(&start).unwrap();
        assert!(matches!(
            started.state(),
            ResidentSandboxGenerationState::MaterializeStarted { .. }
        ));
        assert!(started.plan_materialize().is_err());
        assert!(started.record_prestart_failure(&plan).is_err());
        let revalidate = started.require_revalidation().unwrap();
        assert!(matches!(
            revalidate.state(),
            ResidentSandboxGenerationState::RevalidateRequired {
                attempt_generation: Some(_)
            }
        ));
    }

    #[test]
    fn prestart_failure_retains_history_and_allocates_new_attempt() {
        let intent = intent("sandbox-a", 7);
        let exact_request = request(intent.lima_locator().clone(), "/tmp/lima-a");
        let accepted = intent.accept(&exact_request).unwrap();
        let first = accepted.plan_materialize().unwrap();
        let authorized = accepted.record_materialize_authorized(&first).unwrap();
        let retryable = authorized.record_prestart_failure(&first).unwrap();
        assert!(matches!(
            retryable.state(),
            ResidentSandboxGenerationState::Accepted
        ));
        let second = retryable.plan_materialize().unwrap();
        assert_eq!(first.attempt_generation().get(), 1);
        assert_eq!(second.attempt_generation().get(), 2);
        assert!(retryable.record_materialize_authorized(&first).is_err());
    }

    #[test]
    fn another_generation_cannot_reuse_a_materialize_plan() {
        let first_intent = intent("sandbox-a", 7);
        let first_request = request(first_intent.lima_locator().clone(), "/tmp/lima-a");
        let first = first_intent.accept(&first_request).unwrap();
        let plan = first.plan_materialize().unwrap();

        let second_intent = intent("sandbox-a", 8);
        let second_request = request(second_intent.lima_locator().clone(), "/tmp/lima-a");
        let second = second_intent.accept(&second_request).unwrap();
        assert_ne!(first.lima_locator(), second.lima_locator());
        assert_eq!(
            second.record_materialize_authorized(&plan).unwrap_err().code,
            "resident_sandbox_plan_mismatch"
        );
    }
}
