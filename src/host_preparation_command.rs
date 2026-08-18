use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::host_preparation_plan::{
    ExecutableHostPreparationPhase, HostPreparationProposal, HostPreparationResult,
};

pub const HOST_PREPARATION_COMMAND_SCHEMA_VERSION: u8 = 2;
pub const HOST_PREPARATION_CONFIRMATION_PREFIX: &str = "host-preparation-v2.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationCommandDisposition {
    Ready,
    Blocked,
    ConfirmationRequired,
    ConfirmationMismatch,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPreparationConfirmation {
    schema_version: u8,
    value: String,
}

impl HostPreparationConfirmation {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// One constructor-enforced decision for an exact public host-preparation proposal.
///
/// All state stays private so callers cannot mutate a required decision into `Confirmed` or replace
/// its proposal or expected confirmation. Use [`decide_host_preparation`] to create values and the
/// read-only accessors to inspect them.
#[derive(Debug, Clone, Serialize)]
pub struct HostPreparationCommandDecision {
    schema_version: u8,
    disposition: HostPreparationCommandDisposition,
    proposal: HostPreparationProposal,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmation: Option<HostPreparationConfirmation>,
}

impl HostPreparationCommandDecision {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(&self) -> HostPreparationCommandDisposition {
        self.disposition
    }

    #[must_use]
    pub fn proposal(&self) -> &HostPreparationProposal {
        &self.proposal
    }

    #[must_use]
    pub fn confirmation(&self) -> Option<&HostPreparationConfirmation> {
        self.confirmation.as_ref()
    }

    #[must_use]
    pub fn into_proposal(self) -> HostPreparationProposal {
        self.proposal
    }

    #[must_use]
    pub fn confirmed_phase(&self) -> Option<&ExecutableHostPreparationPhase> {
        if self.disposition != HostPreparationCommandDisposition::Confirmed {
            return None;
        }
        match &self.proposal.result {
            HostPreparationResult::Executable { phase, .. } => Some(phase),
            HostPreparationResult::Ready | HostPreparationResult::Blocked { .. } => None,
        }
    }

    #[must_use]
    pub fn into_confirmed_phase(self) -> Option<ExecutableHostPreparationPhase> {
        if self.disposition != HostPreparationCommandDisposition::Confirmed {
            return None;
        }
        match self.proposal.result {
            HostPreparationResult::Executable { phase, .. } => Some(phase),
            HostPreparationResult::Ready | HostPreparationResult::Blocked { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationCommandErrorKind {
    ProposalSerialization,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPreparationCommandError {
    kind: HostPreparationCommandErrorKind,
    public_message: String,
}

impl HostPreparationCommandError {
    #[must_use]
    pub const fn kind(&self) -> HostPreparationCommandErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn serialization() -> Self {
        Self {
            kind: HostPreparationCommandErrorKind::ProposalSerialization,
            public_message:
                "the public host-preparation proposal could not be encoded for confirmation"
                    .to_owned(),
        }
    }
}

impl fmt::Display for HostPreparationCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for HostPreparationCommandError {}

pub fn host_preparation_confirmation(
    proposal: &HostPreparationProposal,
) -> Result<HostPreparationConfirmation, HostPreparationCommandError> {
    #[derive(Serialize)]
    struct ConfirmationBinding<'a> {
        public_proposal: &'a HostPreparationProposal,
        durable_plan_sha256: Option<String>,
    }

    let durable_plan_sha256 = match &proposal.result {
        HostPreparationResult::Executable { phase, .. } => {
            let exact_plan = serde_json::to_vec(&phase.durable_plan())
                .map_err(|_| HostPreparationCommandError::serialization())?;
            Some(hex_encode(&Sha256::digest(exact_plan)))
        }
        HostPreparationResult::Ready | HostPreparationResult::Blocked { .. } => None,
    };
    let confirmation_binding = serde_json::to_vec(&ConfirmationBinding {
        public_proposal: proposal,
        durable_plan_sha256,
    })
    .map_err(|_| HostPreparationCommandError::serialization())?;
    Ok(HostPreparationConfirmation {
        schema_version: HOST_PREPARATION_COMMAND_SCHEMA_VERSION,
        value: format!(
            "{HOST_PREPARATION_CONFIRMATION_PREFIX}{}",
            hex_encode(&confirmation_binding)
        ),
    })
}

pub fn decide_host_preparation(
    proposal: HostPreparationProposal,
    supplied_confirmation: Option<&str>,
) -> Result<HostPreparationCommandDecision, HostPreparationCommandError> {
    let (disposition, confirmation) = match &proposal.result {
        HostPreparationResult::Ready => (HostPreparationCommandDisposition::Ready, None),
        HostPreparationResult::Blocked { .. } => (HostPreparationCommandDisposition::Blocked, None),
        HostPreparationResult::Executable { .. } => {
            let confirmation = host_preparation_confirmation(&proposal)?;
            let disposition = match supplied_confirmation {
                None => HostPreparationCommandDisposition::ConfirmationRequired,
                Some(supplied) if supplied == confirmation.value.as_str() => {
                    HostPreparationCommandDisposition::Confirmed
                }
                Some(_) => HostPreparationCommandDisposition::ConfirmationMismatch,
            };
            (disposition, Some(confirmation))
        }
    };
    Ok(HostPreparationCommandDecision {
        schema_version: HOST_PREPARATION_COMMAND_SCHEMA_VERSION,
        disposition,
        proposal,
        confirmation,
    })
}

#[must_use]
pub fn render_human(decision: &HostPreparationCommandDecision) -> String {
    let mut output = crate::host_preparation_plan::render_human(&decision.proposal);
    match decision.disposition {
        HostPreparationCommandDisposition::Ready => {
            output.push_str("\nDecision: host preparation is already ready.\n");
        }
        HostPreparationCommandDisposition::Blocked => {
            output.push_str("\nDecision: host preparation is blocked.\n");
        }
        HostPreparationCommandDisposition::ConfirmationRequired => {
            output.push_str("\nDecision: exact irreversible confirmation is required.\n");
            append_confirmation(&mut output, decision.confirmation.as_ref());
        }
        HostPreparationCommandDisposition::ConfirmationMismatch => {
            output.push_str("\nDecision: supplied confirmation mismatches this proposal.\n");
            append_confirmation(&mut output, decision.confirmation.as_ref());
        }
        HostPreparationCommandDisposition::Confirmed => {
            let phase = decision
                .confirmed_phase()
                .expect("confirmed decisions contain one executable phase");
            output.push_str(&format!(
                "\nDecision: irreversible confirmation accepted for phase {} ({} actions).\nThe pure decision layer leaves execution pending.\n",
                phase.id,
                phase.actions.len()
            ));
        }
    }
    output
}

fn append_confirmation(output: &mut String, confirmation: Option<&HostPreparationConfirmation>) {
    if let Some(confirmation) = confirmation {
        output.push_str(&format!("Exact confirmation: {}\n", confirmation.value));
    }
}

fn hex_encode(input: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(input.len().saturating_mul(2));
    for byte in input {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

impl fmt::Display for HostPreparationCommandDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&render_human(self))
    }
}

#[cfg(test)]
mod tests;
