use std::fmt;

use serde::Serialize;

use crate::lane_command::RunnerUserContext;
use crate::process::CommandExecutor;
use crate::runner_account_observation::{RunnerAccountObservationPaths, observe_runner_account};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PreparationObservationState, RunnerAccountObservations,
};
use crate::runner_user::{
    RuntimeDirectoryObservation, VerifiedRunnerUser, inspect_runtime_directory,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshRunnerUserEvidenceErrorKind {
    AccountObservation,
    AccountNotMatching,
    IdentityUnavailable,
    RuntimeDirectory,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FreshRunnerUserEvidenceError {
    kind: FreshRunnerUserEvidenceErrorKind,
    public_message: String,
}

impl FreshRunnerUserEvidenceError {
    #[must_use]
    pub const fn kind(&self) -> FreshRunnerUserEvidenceErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: FreshRunnerUserEvidenceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for FreshRunnerUserEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for FreshRunnerUserEvidenceError {}

/// Re-observe and seal the exact runner-user evidence required by the migration lane.
///
/// The account observer performs fresh NSS, home, subordinate-authority, and linger checks. This
/// adapter then inspects `/run/user/UID` and constructs `VerifiedRunnerUser` only from a completely
/// matching observation. It performs no mutation and accepts no caller-supplied identity receipt.
///
/// # Errors
///
/// Returns a bounded error unless every reviewed account observation, the exact primary group,
/// runtime-directory ownership and mode, and both subordinate allocations match.
pub fn observe_verified_runner_user(
    desired: &DesiredRunnerAccount,
    executor: &impl CommandExecutor,
) -> Result<VerifiedRunnerUser, FreshRunnerUserEvidenceError> {
    let report = observe_runner_account(
        desired,
        executor,
        &RunnerAccountObservationPaths::system_default(),
    )
    .map_err(|_| {
        FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::AccountObservation,
            "fresh runner-user account evidence could not be observed safely",
        )
    })?;
    let identity = report
        .identity()
        .map(|identity| (identity.uid(), identity.primary_gid(), identity.group_gid()));
    let identity = require_matching_account(&report.observations, identity)?;
    let context = RunnerUserContext::new(
        desired.username().clone(),
        identity.0,
        identity.1,
        desired.home(),
    )
    .map_err(|_| {
        FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::IdentityUnavailable,
            "fresh runner-user identity could not be represented by the reviewed execution context",
        )
    })?;
    let runtime = inspect_runtime_directory(&context).map_err(|_| {
        FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::RuntimeDirectory,
            "fresh runner-user runtime-directory evidence did not satisfy reviewed policy",
        )
    })?;
    bind_verified_runner_user(desired, &report.observations, Some(identity), runtime)
}

fn require_matching_account(
    observations: &RunnerAccountObservations,
    identity: Option<(u32, u32, u32)>,
) -> Result<(u32, u32, u32), FreshRunnerUserEvidenceError> {
    let states = [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ];
    if states
        .iter()
        .any(|state| *state != PreparationObservationState::Matching)
    {
        return Err(FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::AccountNotMatching,
            "fresh runner-user account, home, subordinate-ID, and linger evidence must all match before runner-user execution",
        ));
    }
    let Some(identity) = identity else {
        return Err(FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::IdentityUnavailable,
            "fresh runner-user identity evidence is unavailable",
        ));
    };
    if identity.1 != identity.2 {
        return Err(FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::IdentityUnavailable,
            "fresh runner-user primary-group evidence is inconsistent",
        ));
    }
    Ok(identity)
}

fn bind_verified_runner_user(
    desired: &DesiredRunnerAccount,
    observations: &RunnerAccountObservations,
    identity: Option<(u32, u32, u32)>,
    runtime: RuntimeDirectoryObservation,
) -> Result<VerifiedRunnerUser, FreshRunnerUserEvidenceError> {
    let (uid, primary_gid, group_gid) = require_matching_account(observations, identity)?;
    let context = RunnerUserContext::new(
        desired.username().clone(),
        uid,
        primary_gid,
        desired.home(),
    )
    .map_err(|_| {
        FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::IdentityUnavailable,
            "fresh runner-user identity could not be represented by the reviewed execution context",
        )
    })?;
    VerifiedRunnerUser::from_fresh_account_observation(
        &context,
        group_gid,
        runtime,
        desired.subordinate_uids().start(),
        desired.subordinate_uids().count(),
        desired.subordinate_gids().start(),
        desired.subordinate_gids().count(),
    )
    .map_err(|_| {
        FreshRunnerUserEvidenceError::new(
            FreshRunnerUserEvidenceErrorKind::Verification,
            "fresh runner-user evidence failed final sealed verification",
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::lane_command::LinuxAccountName;
    use crate::runner_account_plan::{
        DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
        PreparationObservationState, RunnerAccountObservations,
    };
    use crate::runner_user::RuntimeDirectoryObservation;

    use super::{FreshRunnerUserEvidenceErrorKind, bind_verified_runner_user};

    fn desired(uid_start: u32) -> DesiredRunnerAccount {
        DesiredRunnerAccount::new(
            LinuxAccountName::parse("project-runner").expect("username"),
            LinuxAccountName::parse("project-runner").expect("group"),
            "/var/lib/project-runner",
            PlannedSubordinateRange::new(uid_start, 65_536).expect("subuid"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("subgid"),
        )
        .expect("desired runner")
    }

    fn observation(state: PreparationObservationState, label: &str) -> PreparationObservation {
        PreparationObservation::new(state, [format!("observed {label}")]).expect("observation")
    }

    fn observations() -> RunnerAccountObservations {
        RunnerAccountObservations {
            group: observation(PreparationObservationState::Matching, "group"),
            user: observation(PreparationObservationState::Matching, "user"),
            home: observation(PreparationObservationState::Matching, "home"),
            subordinate_uids: observation(
                PreparationObservationState::Matching,
                "subordinate UIDs",
            ),
            subordinate_gids: observation(
                PreparationObservationState::Matching,
                "subordinate GIDs",
            ),
            linger: observation(PreparationObservationState::Matching, "linger"),
        }
    }

    fn runtime(mode: u32) -> RuntimeDirectoryObservation {
        RuntimeDirectoryObservation::for_test("/run/user/1001", 1001, 1001, mode)
    }

    #[test]
    fn binds_only_complete_matching_fresh_evidence() {
        let verified = bind_verified_runner_user(
            &desired(100_000),
            &observations(),
            Some((1001, 1001, 1001)),
            runtime(0o700),
        )
        .expect("verified runner user");
        assert_eq!(verified.username().as_str(), "project-runner");
        assert_eq!(verified.uid(), 1001);
        assert_eq!(verified.primary_gid(), 1001);
        assert_eq!(verified.subordinate_uid_count(), 65_536);
        assert_eq!(verified.subordinate_gid_count(), 65_536);
    }

    #[test]
    fn refuses_nonmatching_account_or_primary_group_evidence() {
        let mut observed = observations();
        observed.linger = observation(PreparationObservationState::Absent, "linger");
        let error = bind_verified_runner_user(
            &desired(100_000),
            &observed,
            Some((1001, 1001, 1001)),
            runtime(0o700),
        )
        .expect_err("absent linger must fail");
        assert_eq!(
            error.kind(),
            FreshRunnerUserEvidenceErrorKind::AccountNotMatching
        );

        let error = bind_verified_runner_user(
            &desired(100_000),
            &observations(),
            Some((1001, 1001, 1002)),
            runtime(0o700),
        )
        .expect_err("group mismatch must fail");
        assert_eq!(
            error.kind(),
            FreshRunnerUserEvidenceErrorKind::IdentityUnavailable
        );
    }

    #[test]
    fn refuses_runtime_or_subordinate_identity_overlap() {
        let error = bind_verified_runner_user(
            &desired(100_000),
            &observations(),
            Some((1001, 1001, 1001)),
            runtime(0o755),
        )
        .expect_err("broad runtime mode must fail");
        assert_eq!(error.kind(), FreshRunnerUserEvidenceErrorKind::Verification);

        let error = bind_verified_runner_user(
            &desired(1000),
            &observations(),
            Some((1001, 1001, 1001)),
            runtime(0o700),
        )
        .expect_err("own UID overlap must fail");
        assert_eq!(error.kind(), FreshRunnerUserEvidenceErrorKind::Verification);
    }
}
