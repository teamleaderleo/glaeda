use std::fmt;
use std::path::{Component, Path};

use serde::Serialize;

use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
use crate::lane_command::{
    LaneCommand, LaneCommandError, LinuxAccountName,
};
use crate::runner_user::MIN_SUBORDINATE_ID_COUNT;

pub const RUNNER_ACCOUNT_PLAN_SCHEMA_VERSION: u8 = 1;
const MAX_EVIDENCE_ITEMS: usize = 64;
const MAX_EVIDENCE_LEN: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlannedSubordinateRange {
    start: u32,
    count: u32,
}

impl PlannedSubordinateRange {
    /// Build one exact subordinate-ID range for a reviewed `usermod` command.
    ///
    /// # Errors
    ///
    /// Returns an error when the range begins at zero, contains fewer than 65,536 IDs, or
    /// overflows the 32-bit Linux ID space.
    pub fn new(start: u32, count: u32) -> Result<Self, RunnerAccountPlanError> {
        let end_exclusive = u64::from(start) + u64::from(count);
        if start == 0
            || u64::from(count) < MIN_SUBORDINATE_ID_COUNT
            || end_exclusive > u64::from(u32::MAX) + 1
        {
            return Err(RunnerAccountPlanError::single(
                "subordinate-ID range must begin above zero, contain at least 65536 IDs, and remain within the 32-bit ID space",
            ));
        }
        Ok(Self { start, count })
    }

    #[must_use]
    pub fn start(&self) -> u32 {
        self.start
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }

    #[must_use]
    pub fn end_inclusive(&self) -> u32 {
        let end = u64::from(self.start) + u64::from(self.count) - 1;
        u32::try_from(end).expect("validated subordinate range ends within u32")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesiredRunnerAccount {
    username: LinuxAccountName,
    primary_group: LinuxAccountName,
    home: String,
    subordinate_uids: PlannedSubordinateRange,
    subordinate_gids: PlannedSubordinateRange,
}

impl DesiredRunnerAccount {
    /// Build one reviewed runner-account identity and exact subordinate-ID allocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the home is not a canonical non-root absolute path.
    pub fn new(
        username: LinuxAccountName,
        primary_group: LinuxAccountName,
        home: &str,
        subordinate_uids: PlannedSubordinateRange,
        subordinate_gids: PlannedSubordinateRange,
    ) -> Result<Self, RunnerAccountPlanError> {
        let home = canonical_home(home)?;
        Ok(Self {
            username,
            primary_group,
            home,
            subordinate_uids,
            subordinate_gids,
        })
    }

    #[must_use]
    pub fn username(&self) -> &LinuxAccountName {
        &self.username
    }

    #[must_use]
    pub fn primary_group(&self) -> &LinuxAccountName {
        &self.primary_group
    }

    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    #[must_use]
    pub fn subordinate_uids(&self) -> PlannedSubordinateRange {
        self.subordinate_uids
    }

    #[must_use]
    pub fn subordinate_gids(&self) -> PlannedSubordinateRange {
        self.subordinate_gids
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationObservationState {
    Matching,
    Absent,
    Unknown,
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreparationObservation {
    state: PreparationObservationState,
    evidence: Vec<String>,
}

impl PreparationObservation {
    /// Record one classified resource observation with bounded public evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, empty, control-bearing, or oversized evidence.
    pub fn new(
        state: PreparationObservationState,
        evidence: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, RunnerAccountPlanError> {
        let evidence = evidence.into_iter().map(Into::into).collect::<Vec<_>>();
        validate_evidence(&evidence)?;
        Ok(Self { state, evidence })
    }

    #[must_use]
    pub fn state(&self) -> PreparationObservationState {
        self.state
    }

    #[must_use]
    pub fn evidence(&self) -> &[String] {
        &self.evidence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountObservations {
    pub group: PreparationObservation,
    pub user: PreparationObservation,
    pub home: PreparationObservation,
    pub subordinate_uids: PreparationObservation,
    pub subordinate_gids: PreparationObservation,
    pub linger: PreparationObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerAccountResourceKind {
    Group,
    User,
    HomeDirectory,
    SubordinateUids,
    SubordinateGids,
    Linger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerAccountPlanDisposition {
    Satisfied,
    Required,
    NeedsInspection,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountPlanItem {
    pub kind: RunnerAccountResourceKind,
    pub disposition: RunnerAccountPlanDisposition,
    pub summary: String,
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<PlannedMutation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<LaneCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountPlan {
    pub schema_version: u8,
    pub desired: DesiredRunnerAccount,
    pub items: Vec<RunnerAccountPlanItem>,
}

/// Build an ordered, dependency-aware runner account preparation plan.
///
/// A matching user is accepted only when its primary group is also matching. Matching home,
/// subordinate-ID, or linger observations require a matching user. Unknown or conflicting group
/// and user identity blocks every dependent mutation. Proven absence may produce exact root-lane
/// commands; this function never executes them.
///
/// # Errors
///
/// Returns an error for inconsistent observation classifications or command construction failure.
pub fn build_runner_account_plan(
    desired: DesiredRunnerAccount,
    observed: RunnerAccountObservations,
) -> Result<RunnerAccountPlan, RunnerAccountPlanError> {
    validate_cross_resource_state(&observed)?;

    let mut items = Vec::with_capacity(6);
    items.push(plan_group(&desired, &observed.group)?);

    let group_viable = is_viable(observed.group.state);
    items.push(if group_viable {
        plan_user(&desired, &observed.user)?
    } else {
        blocked_item(
            RunnerAccountResourceKind::User,
            format!("ensure dedicated runner user {}", desired.username.as_str()),
            &observed.user,
            "runner group identity is not safe to use",
        )
    });

    let user_viable = group_viable && is_viable(observed.user.state);
    items.push(if user_viable {
        plan_home(&desired, &observed.home)?
    } else {
        blocked_item(
            RunnerAccountResourceKind::HomeDirectory,
            format!("ensure runner home directory {}", desired.home),
            &observed.home,
            "runner user identity is not safe to use",
        )
    });
    items.push(if user_viable {
        plan_subordinate_uids(&desired, &observed.subordinate_uids)?
    } else {
        blocked_item(
            RunnerAccountResourceKind::SubordinateUids,
            format!(
                "ensure subordinate UID range {}-{} for {}",
                desired.subordinate_uids.start(),
                desired.subordinate_uids.end_inclusive(),
                desired.username.as_str()
            ),
            &observed.subordinate_uids,
            "runner user identity is not safe to use",
        )
    });
    items.push(if user_viable {
        plan_subordinate_gids(&desired, &observed.subordinate_gids)?
    } else {
        blocked_item(
            RunnerAccountResourceKind::SubordinateGids,
            format!(
                "ensure subordinate GID range {}-{} for {}",
                desired.subordinate_gids.start(),
                desired.subordinate_gids.end_inclusive(),
                desired.username.as_str()
            ),
            &observed.subordinate_gids,
            "runner user identity is not safe to use",
        )
    });
    items.push(if user_viable {
        plan_linger(&desired, &observed.linger)?
    } else {
        blocked_item(
            RunnerAccountResourceKind::Linger,
            format!("ensure systemd linger for {}", desired.username.as_str()),
            &observed.linger,
            "runner user identity is not safe to use",
        )
    });

    Ok(RunnerAccountPlan {
        schema_version: RUNNER_ACCOUNT_PLAN_SCHEMA_VERSION,
        desired,
        items,
    })
}

#[must_use]
pub fn render_human(plan: &RunnerAccountPlan) -> String {
    let mut output = format!(
        "SmolRunner runner account plan\n\nRunner user: {}\nPrimary group: {}\nHome: {}\n\n",
        plan.desired.username.as_str(),
        plan.desired.primary_group.as_str(),
        plan.desired.home
    );
    for item in &plan.items {
        let marker = match item.disposition {
            RunnerAccountPlanDisposition::Satisfied => "READY",
            RunnerAccountPlanDisposition::Required => "REQUIRED",
            RunnerAccountPlanDisposition::NeedsInspection => "INSPECT",
            RunnerAccountPlanDisposition::Blocked => "BLOCKED",
        };
        output.push_str(&format!("[{marker}] {}\n", item.summary));
        if let Some(command) = &item.command {
            output.push_str("  Reviewed command: ");
            output.push_str(&command.spec().displayed_argv().join(" "));
            output.push('\n');
        }
    }
    output.push_str("\nNo changes were made.\n");
    output
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountPlanError {
    pub problems: Vec<String>,
}

impl RunnerAccountPlanError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl From<LaneCommandError> for RunnerAccountPlanError {
    fn from(error: LaneCommandError) -> Self {
        Self {
            problems: error.problems,
        }
    }
}

impl fmt::Display for RunnerAccountPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "runner account plan validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RunnerAccountPlanError {}

fn plan_group(
    desired: &DesiredRunnerAccount,
    observed: &PreparationObservation,
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let summary = format!(
        "ensure dedicated runner group {}",
        desired.primary_group.as_str()
    );
    plan_item(
        RunnerAccountResourceKind::Group,
        "ensure-runner-group",
        summary,
        observed,
        |mutation| LaneCommand::ensure_system_group(mutation, &desired.primary_group),
        [format!(
            "desired runner group {}",
            desired.primary_group.as_str()
        )],
    )
}

fn plan_user(
    desired: &DesiredRunnerAccount,
    observed: &PreparationObservation,
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let summary = format!("ensure dedicated runner user {}", desired.username.as_str());
    plan_item(
        RunnerAccountResourceKind::User,
        "ensure-runner-user",
        summary,
        observed,
        |mutation| {
            LaneCommand::ensure_system_user(
                mutation,
                &desired.username,
                &desired.primary_group,
                &desired.home,
            )
        },
        [
            format!("desired runner user {}", desired.username.as_str()),
            format!(
                "desired primary group {}",
                desired.primary_group.as_str()
            ),
            format!("desired home {}", desired.home),
            "desired shell /usr/sbin/nologin".to_owned(),
        ],
    )
}

fn plan_home(
    desired: &DesiredRunnerAccount,
    observed: &PreparationObservation,
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let summary = format!("ensure runner home directory {}", desired.home);
    plan_item(
        RunnerAccountResourceKind::HomeDirectory,
        "ensure-runner-home",
        summary,
        observed,
        |mutation| {
            LaneCommand::ensure_home_directory(
                mutation,
                &desired.username,
                &desired.primary_group,
                &desired.home,
            )
        },
        [
            format!("desired home {}", desired.home),
            format!("desired owner {}", desired.username.as_str()),
            format!("desired group {}", desired.primary_group.as_str()),
            "desired mode 0750".to_owned(),
        ],
    )
}

fn plan_subordinate_uids(
    desired: &DesiredRunnerAccount,
    observed: &PreparationObservation,
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let range = desired.subordinate_uids;
    let summary = format!(
        "ensure subordinate UID range {}-{} for {}",
        range.start(),
        range.end_inclusive(),
        desired.username.as_str()
    );
    plan_item(
        RunnerAccountResourceKind::SubordinateUids,
        "ensure-runner-subordinate-uids",
        summary,
        observed,
        |mutation| {
            LaneCommand::ensure_subordinate_uids(
                mutation,
                &desired.username,
                range.start(),
                range.count(),
            )
        },
        [format!(
            "desired subordinate UID range {}-{}",
            range.start(),
            range.end_inclusive()
        )],
    )
}

fn plan_subordinate_gids(
    desired: &DesiredRunnerAccount,
    observed: &PreparationObservation,
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let range = desired.subordinate_gids;
    let summary = format!(
        "ensure subordinate GID range {}-{} for {}",
        range.start(),
        range.end_inclusive(),
        desired.username.as_str()
    );
    plan_item(
        RunnerAccountResourceKind::SubordinateGids,
        "ensure-runner-subordinate-gids",
        summary,
        observed,
        |mutation| {
            LaneCommand::ensure_subordinate_gids(
                mutation,
                &desired.username,
                range.start(),
                range.count(),
            )
        },
        [format!(
            "desired subordinate GID range {}-{}",
            range.start(),
            range.end_inclusive()
        )],
    )
}

fn plan_linger(
    desired: &DesiredRunnerAccount,
    observed: &PreparationObservation,
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let summary = format!("ensure systemd linger for {}", desired.username.as_str());
    plan_item(
        RunnerAccountResourceKind::Linger,
        "enable-runner-linger",
        summary,
        observed,
        |mutation| LaneCommand::enable_linger(mutation, &desired.username),
        [format!(
            "desired linger enabled for {}",
            desired.username.as_str()
        )],
    )
}

fn plan_item<const N: usize>(
    kind: RunnerAccountResourceKind,
    action_id: &str,
    summary: String,
    observed: &PreparationObservation,
    command: impl FnOnce(&PlannedMutation) -> Result<LaneCommand, LaneCommandError>,
    desired_evidence: [String; N],
) -> Result<RunnerAccountPlanItem, RunnerAccountPlanError> {
    let mut evidence = desired_evidence.into_iter().collect::<Vec<_>>();
    evidence.extend(observed.evidence.iter().cloned());
    match observed.state {
        PreparationObservationState::Matching => Ok(RunnerAccountPlanItem {
            kind,
            disposition: RunnerAccountPlanDisposition::Satisfied,
            summary,
            evidence,
            mutation: None,
            command: None,
        }),
        PreparationObservationState::Absent => {
            let mutation = PlannedMutation::new(
                action_id,
                ExecutionLane::Root,
                summary.clone(),
                RollbackClass::Compensating,
                Preconditions::new(evidence.clone()),
            );
            let command = command(&mutation)?;
            Ok(RunnerAccountPlanItem {
                kind,
                disposition: RunnerAccountPlanDisposition::Required,
                summary,
                evidence,
                mutation: Some(mutation),
                command: Some(command),
            })
        }
        PreparationObservationState::Unknown => Ok(RunnerAccountPlanItem {
            kind,
            disposition: RunnerAccountPlanDisposition::NeedsInspection,
            summary,
            evidence,
            mutation: None,
            command: None,
        }),
        PreparationObservationState::Conflicting => Ok(RunnerAccountPlanItem {
            kind,
            disposition: RunnerAccountPlanDisposition::Blocked,
            summary,
            evidence,
            mutation: None,
            command: None,
        }),
    }
}

fn blocked_item(
    kind: RunnerAccountResourceKind,
    summary: String,
    observed: &PreparationObservation,
    reason: &str,
) -> RunnerAccountPlanItem {
    let mut evidence = observed.evidence.clone();
    evidence.push(reason.to_owned());
    RunnerAccountPlanItem {
        kind,
        disposition: RunnerAccountPlanDisposition::Blocked,
        summary,
        evidence,
        mutation: None,
        command: None,
    }
}

fn is_viable(state: PreparationObservationState) -> bool {
    matches!(
        state,
        PreparationObservationState::Matching | PreparationObservationState::Absent
    )
}

fn validate_cross_resource_state(
    observed: &RunnerAccountObservations,
) -> Result<(), RunnerAccountPlanError> {
    if observed.user.state == PreparationObservationState::Matching
        && observed.group.state != PreparationObservationState::Matching
    {
        return Err(RunnerAccountPlanError::single(
            "a matching runner user requires a matching primary group observation",
        ));
    }
    if observed.user.state != PreparationObservationState::Matching {
        for (field, state) in [
            ("home", observed.home.state),
            ("subordinate UIDs", observed.subordinate_uids.state),
            ("subordinate GIDs", observed.subordinate_gids.state),
            ("linger", observed.linger.state),
        ] {
            if state == PreparationObservationState::Matching {
                return Err(RunnerAccountPlanError::single(format!(
                    "a matching {field} observation requires a matching runner user"
                )));
            }
        }
    }
    Ok(())
}

fn validate_evidence(evidence: &[String]) -> Result<(), RunnerAccountPlanError> {
    if evidence.is_empty() || evidence.len() > MAX_EVIDENCE_ITEMS {
        return Err(RunnerAccountPlanError::single(format!(
            "observation evidence must contain 1 to {MAX_EVIDENCE_ITEMS} entries"
        )));
    }
    for item in evidence {
        if item.is_empty()
            || item.len() > MAX_EVIDENCE_LEN
            || item.chars().any(char::is_control)
        {
            return Err(RunnerAccountPlanError::single(format!(
                "observation evidence entries must be nonempty, contain no control characters, and not exceed {MAX_EVIDENCE_LEN} bytes"
            )));
        }
    }
    Ok(())
}

fn canonical_home(value: &str) -> Result<String, RunnerAccountPlanError> {
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.chars().any(char::is_control)
    {
        return Err(RunnerAccountPlanError::single(
            "runner home must be a canonical non-root absolute path",
        ));
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(RunnerAccountPlanError::single(
            "runner home must be a canonical non-root absolute path without aliases",
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use crate::journal::{ExecutionLane, RollbackClass};
    use crate::lane_command::LinuxAccountName;

    use super::{
        DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
        PreparationObservationState, RunnerAccountObservations, RunnerAccountPlanDisposition,
        RunnerAccountResourceKind, build_runner_account_plan, render_human,
    };

    fn account(name: &str) -> LinuxAccountName {
        LinuxAccountName::parse(name).expect("valid account name")
    }

    fn desired() -> DesiredRunnerAccount {
        DesiredRunnerAccount::new(
            account("project-runner"),
            account("project-runner"),
            "/var/lib/project-runner",
            PlannedSubordinateRange::new(100_000, 65_536).expect("subuid range"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("subgid range"),
        )
        .expect("desired runner account")
    }

    fn observation(state: PreparationObservationState, name: &str) -> PreparationObservation {
        PreparationObservation::new(state, [format!("observed {name} state")])
            .expect("observation")
    }

    fn observations(state: PreparationObservationState) -> RunnerAccountObservations {
        RunnerAccountObservations {
            group: observation(state, "group"),
            user: observation(state, "user"),
            home: observation(state, "home"),
            subordinate_uids: observation(state, "subordinate UIDs"),
            subordinate_gids: observation(state, "subordinate GIDs"),
            linger: observation(state, "linger"),
        }
    }

    #[test]
    fn matching_state_is_fully_satisfied() {
        let plan = build_runner_account_plan(
            desired(),
            observations(PreparationObservationState::Matching),
        )
        .expect("matching plan");
        assert_eq!(plan.items.len(), 6);
        assert!(plan.items.iter().all(|item| {
            item.disposition == RunnerAccountPlanDisposition::Satisfied
                && item.mutation.is_none()
                && item.command.is_none()
        }));
        assert_eq!(render_human(&plan).matches("No changes were made.").count(), 1);
    }

    #[test]
    fn proven_absence_builds_six_ordered_compensating_root_commands() {
        let plan = build_runner_account_plan(
            desired(),
            observations(PreparationObservationState::Absent),
        )
        .expect("absent plan");
        assert_eq!(
            plan.items.iter().map(|item| item.kind).collect::<Vec<_>>(),
            [
                RunnerAccountResourceKind::Group,
                RunnerAccountResourceKind::User,
                RunnerAccountResourceKind::HomeDirectory,
                RunnerAccountResourceKind::SubordinateUids,
                RunnerAccountResourceKind::SubordinateGids,
                RunnerAccountResourceKind::Linger,
            ]
        );
        assert!(plan.items.iter().all(|item| {
            item.disposition == RunnerAccountPlanDisposition::Required
                && item.mutation.as_ref().is_some_and(|mutation| {
                    mutation.lane == ExecutionLane::Root
                        && mutation.rollback == RollbackClass::Compensating
                })
                && item.command.is_some()
        }));
        assert_eq!(
            plan.items[2]
                .command
                .as_ref()
                .expect("home command")
                .spec()
                .displayed_argv(),
            [
                "/usr/bin/install",
                "--directory",
                "--mode",
                "0750",
                "--owner",
                "project-runner",
                "--group",
                "project-runner",
                "--",
                "/var/lib/project-runner",
            ]
        );
        assert_eq!(
            plan.items[3]
                .command
                .as_ref()
                .expect("subuid command")
                .spec()
                .displayed_argv(),
            [
                "/usr/sbin/usermod",
                "--add-subuids",
                "100000-165535",
                "--",
                "project-runner",
            ]
        );
    }

    #[test]
    fn unknown_group_blocks_all_dependent_actions() {
        let mut observed = observations(PreparationObservationState::Absent);
        observed.group = observation(PreparationObservationState::Unknown, "group");
        let plan = build_runner_account_plan(desired(), observed).expect("blocked plan");
        assert_eq!(
            plan.items[0].disposition,
            RunnerAccountPlanDisposition::NeedsInspection
        );
        assert!(plan.items[1..].iter().all(|item| {
            item.disposition == RunnerAccountPlanDisposition::Blocked
                && item.mutation.is_none()
                && item.command.is_none()
        }));
    }

    #[test]
    fn conflicting_user_blocks_home_ranges_and_linger() {
        let mut observed = observations(PreparationObservationState::Absent);
        observed.group = observation(PreparationObservationState::Matching, "group");
        observed.user = observation(PreparationObservationState::Conflicting, "user");
        let plan = build_runner_account_plan(desired(), observed).expect("blocked plan");
        assert_eq!(plan.items[0].disposition, RunnerAccountPlanDisposition::Satisfied);
        assert_eq!(plan.items[1].disposition, RunnerAccountPlanDisposition::Blocked);
        assert!(plan.items[2..].iter().all(|item| {
            item.disposition == RunnerAccountPlanDisposition::Blocked
                && item.command.is_none()
        }));
    }

    #[test]
    fn inconsistent_matching_dependencies_fail_closed() {
        let mut observed = observations(PreparationObservationState::Absent);
        observed.user = observation(PreparationObservationState::Matching, "user");
        build_runner_account_plan(desired(), observed)
            .expect_err("matching user without matching group");

        let mut observed = observations(PreparationObservationState::Absent);
        observed.home = observation(PreparationObservationState::Matching, "home");
        build_runner_account_plan(desired(), observed)
            .expect_err("matching home without matching user");
    }

    #[test]
    fn unsafe_home_range_and_evidence_are_rejected() {
        DesiredRunnerAccount::new(
            account("project-runner"),
            account("project-runner"),
            "/var/lib/../root",
            PlannedSubordinateRange::new(100_000, 65_536).expect("subuid range"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("subgid range"),
        )
        .expect_err("aliased home");
        PlannedSubordinateRange::new(100_000, 1).expect_err("undersized range");
        PreparationObservation::new(PreparationObservationState::Absent, Vec::<String>::new())
            .expect_err("missing evidence");
    }
}
