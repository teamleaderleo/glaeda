use std::fmt;

use serde::Serialize;

use crate::state::InstallationId;

pub const LEASE_SCHEMA_VERSION: u8 = 1;
const MAX_LEASE_ID_LEN: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LeaseId(String);

impl LeaseId {
    /// Validate one opaque lease identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be used as one bounded durable-state identifier.
    pub fn parse(value: &str) -> Result<Self, LeaseIdError> {
        let mut bytes = value.bytes();
        let first_is_safe = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        let remaining_are_safe = bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });

        if value.len() > MAX_LEASE_ID_LEN || !first_is_safe || !remaining_are_safe {
            return Err(LeaseIdError {
                value: value.to_owned(),
                problem: format!(
                    "must be at most {MAX_LEASE_ID_LEN} lowercase ASCII letters, digits, '.', '_', or '-', beginning with a letter or digit"
                ),
            });
        }

        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseIdError {
    pub value: String,
    pub problem: String,
}

impl fmt::Display for LeaseIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lease ID {:?} {}", self.value, self.problem)
    }
}

impl std::error::Error for LeaseIdError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
    Run,
    Workspace,
    Preview,
}

impl LeaseKind {
    const fn supports_sleep(self) -> bool {
        matches!(self, Self::Workspace | Self::Preview)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Workspace => "workspace",
            Self::Preview => "preview",
        }
    }
}

impl fmt::Display for LeaseKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Pending,
    Active,
    Sleeping,
    Releasing,
    Released,
    Expired,
    Failed,
}

impl LeaseState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Released | Self::Expired | Self::Failed)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Sleeping => "sleeping",
            Self::Releasing => "releasing",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for LeaseState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseAction {
    Activate,
    Renew,
    Sleep,
    Wake,
    BeginRelease,
    FinishRelease,
    Expire,
    Fail,
}

impl LeaseAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Renew => "renew",
            Self::Sleep => "sleep",
            Self::Wake => "wake",
            Self::BeginRelease => "begin_release",
            Self::FinishRelease => "finish_release",
            Self::Expire => "expire",
            Self::Fail => "fail",
        }
    }
}

impl fmt::Display for LeaseAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseIdentity {
    pub lease_id: LeaseId,
    pub installation_id: InstallationId,
    pub kind: LeaseKind,
}

impl LeaseIdentity {
    #[must_use]
    pub const fn new(
        lease_id: LeaseId,
        installation_id: InstallationId,
        kind: LeaseKind,
    ) -> Self {
        Self {
            lease_id,
            installation_id,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseRecord {
    pub schema_version: u8,
    pub identity: LeaseIdentity,
    pub state: LeaseState,
    pub revision: u64,
}

impl LeaseRecord {
    #[must_use]
    pub const fn pending(identity: LeaseIdentity) -> Self {
        Self {
            schema_version: LEASE_SCHEMA_VERSION,
            identity,
            state: LeaseState::Pending,
            revision: 0,
        }
    }

    /// Plan one revision-checked state transition without changing durable state.
    ///
    /// The caller may serialize the returned transition, compare its previous revision with durable
    /// state, and persist `LeaseTransition::resulting_record` through a later compare-and-swap path.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is invalid for the current lease kind or state, or when the
    /// revision counter is exhausted.
    pub fn plan_transition(
        &self,
        action: LeaseAction,
    ) -> Result<LeaseTransition, LeaseTransitionError> {
        let to = next_state(self.identity.kind, self.state, action)
            .map_err(|problem| LeaseTransitionError::new(self, action, problem))?;
        let next_revision = self.revision.checked_add(1).ok_or_else(|| {
            LeaseTransitionError::new(self, action, "revision counter is exhausted")
        })?;

        Ok(LeaseTransition {
            schema_version: LEASE_SCHEMA_VERSION,
            identity: self.identity.clone(),
            from: self.state,
            action,
            to,
            previous_revision: self.revision,
            next_revision,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseTransition {
    pub schema_version: u8,
    pub identity: LeaseIdentity,
    pub from: LeaseState,
    pub action: LeaseAction,
    pub to: LeaseState,
    pub previous_revision: u64,
    pub next_revision: u64,
}

impl LeaseTransition {
    #[must_use]
    pub fn resulting_record(&self) -> LeaseRecord {
        LeaseRecord {
            schema_version: LEASE_SCHEMA_VERSION,
            identity: self.identity.clone(),
            state: self.to,
            revision: self.next_revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseTransitionError {
    pub lease_id: LeaseId,
    pub kind: LeaseKind,
    pub current: LeaseState,
    pub action: LeaseAction,
    pub problem: String,
}

impl LeaseTransitionError {
    fn new(record: &LeaseRecord, action: LeaseAction, problem: impl Into<String>) -> Self {
        Self {
            lease_id: record.identity.lease_id.clone(),
            kind: record.identity.kind,
            current: record.state,
            action,
            problem: problem.into(),
        }
    }
}

impl fmt::Display for LeaseTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} lease {} cannot apply {} while {}: {}",
            self.kind,
            self.lease_id.as_str(),
            self.action,
            self.current,
            self.problem
        )
    }
}

impl std::error::Error for LeaseTransitionError {}

fn next_state(
    kind: LeaseKind,
    state: LeaseState,
    action: LeaseAction,
) -> Result<LeaseState, &'static str> {
    use LeaseAction::{
        Activate, BeginRelease, Expire, Fail, FinishRelease, Renew, Sleep, Wake,
    };
    use LeaseState::{Active, Expired, Failed, Pending, Released, Releasing, Sleeping};

    if state.is_terminal() {
        return Err("terminal leases cannot transition");
    }

    match (state, action) {
        (Pending, Activate) => Ok(Active),
        (Pending | Active | Sleeping, Renew) => Ok(state),
        (Active, Sleep) if kind.supports_sleep() => Ok(Sleeping),
        (Active, Sleep) => Err("run leases cannot sleep"),
        (Sleeping, Wake) => Ok(Active),
        (Pending | Active | Sleeping, BeginRelease) => Ok(Releasing),
        (Releasing, FinishRelease) => Ok(Released),
        (Pending | Active | Sleeping | Releasing, Expire) => Ok(Expired),
        (Pending | Active | Sleeping | Releasing, Fail) => Ok(Failed),
        _ => Err("action is invalid for the current state"),
    }
}

#[cfg(test)]
mod tests {
    use crate::state::InstallationId;

    use super::{LeaseAction, LeaseId, LeaseIdentity, LeaseKind, LeaseRecord, LeaseState};

    fn identity(kind: LeaseKind) -> LeaseIdentity {
        LeaseIdentity::new(
            LeaseId::parse("lease-001").expect("valid lease ID"),
            InstallationId::parse("installation-001").expect("valid installation ID"),
            kind,
        )
    }

    #[test]
    fn lease_ids_are_bounded_state_safe_components() {
        assert!(LeaseId::parse("preview-pr-42").is_ok());
        assert!(LeaseId::parse("").is_err());
        assert!(LeaseId::parse("Preview-42").is_err());
        assert!(LeaseId::parse("../preview").is_err());
    }

    #[test]
    fn workspace_can_activate_sleep_wake_and_release() {
        let pending = LeaseRecord::pending(identity(LeaseKind::Workspace));
        let active = pending
            .plan_transition(LeaseAction::Activate)
            .expect("activate workspace")
            .resulting_record();
        let sleeping = active
            .plan_transition(LeaseAction::Sleep)
            .expect("sleep workspace")
            .resulting_record();
        let awake = sleeping
            .plan_transition(LeaseAction::Wake)
            .expect("wake workspace")
            .resulting_record();
        let releasing = awake
            .plan_transition(LeaseAction::BeginRelease)
            .expect("begin release")
            .resulting_record();
        let released = releasing
            .plan_transition(LeaseAction::FinishRelease)
            .expect("finish release")
            .resulting_record();

        assert_eq!(released.state, LeaseState::Released);
        assert_eq!(released.revision, 5);
        assert!(released.state.is_terminal());
    }

    #[test]
    fn run_lease_rejects_sleep() {
        let active = LeaseRecord::pending(identity(LeaseKind::Run))
            .plan_transition(LeaseAction::Activate)
            .expect("activate run")
            .resulting_record();
        let error = active
            .plan_transition(LeaseAction::Sleep)
            .expect_err("run must reject sleep");

        assert_eq!(error.problem, "run leases cannot sleep");
    }

    #[test]
    fn renewal_preserves_state_and_advances_revision() {
        let active = LeaseRecord::pending(identity(LeaseKind::Preview))
            .plan_transition(LeaseAction::Activate)
            .expect("activate preview")
            .resulting_record();
        let renewal = active
            .plan_transition(LeaseAction::Renew)
            .expect("renew preview");

        assert_eq!(renewal.from, LeaseState::Active);
        assert_eq!(renewal.to, LeaseState::Active);
        assert_eq!(renewal.previous_revision, 1);
        assert_eq!(renewal.next_revision, 2);
    }

    #[test]
    fn terminal_lease_rejects_further_actions() {
        let expired = LeaseRecord::pending(identity(LeaseKind::Preview))
            .plan_transition(LeaseAction::Expire)
            .expect("expire preview")
            .resulting_record();
        let error = expired
            .plan_transition(LeaseAction::Renew)
            .expect_err("terminal lease must reject renewal");

        assert_eq!(error.problem, "terminal leases cannot transition");
    }

    #[test]
    fn revision_overflow_fails_before_emitting_a_transition() {
        let record = LeaseRecord {
            schema_version: super::LEASE_SCHEMA_VERSION,
            identity: identity(LeaseKind::Run),
            state: LeaseState::Pending,
            revision: u64::MAX,
        };
        let error = record
            .plan_transition(LeaseAction::Activate)
            .expect_err("revision overflow must fail");

        assert_eq!(error.problem, "revision counter is exhausted");
    }
}
