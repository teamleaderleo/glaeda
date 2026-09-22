//! Bounded reclamation planning for one checkout-local Cargo target.
//!
//! Authority here comes from reconstructibility, never from proven idleness. Holder observation is
//! positive-only: no count it reports can prove a target is unused, so this module never claims
//! that. A Cargo target rebuilds from the checkout and its lockfile, so reclaiming one that turns
//! out to be in use costs a failed build and a rebuild -- time, never truth. Positive holder
//! evidence is still a veto, because paying a rebuild for nothing has no upside.
//!
//! Planning performs no mutation and emits no paths. It consumes observations the caller took and
//! returns one decision with every veto that applied, so a refusal explains itself completely
//! instead of surfacing only the first reason.

use std::fmt;

use serde::Serialize;

use crate::cargo_target_holder_observation::{
    CargoTargetHolderDisposition, CargoTargetHolderObservation, CargoTargetHolderState,
};
use crate::cargo_target_observation::{
    CargoTargetHardlinkCoverage, CargoTargetObservation, CargoTargetState, RustcInfoObservation,
};

pub const CARGO_TARGET_RECLAIM_SCHEMA_VERSION: u8 = 1;

/// Smallest idle window a policy may require.
///
/// A zero window would let a target be reclaimed in the same second a build wrote to it, which
/// makes the common accident -- reclaiming state a live build is still producing -- reachable
/// through ordinary configuration rather than through an explicit override.
pub const MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS: i64 = 3_600;

/// What the positive-only holder scan actually managed to look at.
///
/// Incomplete process coverage is the normal case, not a fault: on an ordinary multi-user Linux
/// host most `/proc` entries belong to other users and cannot be read, and the process table
/// changes while it is being walked. Measured on one developer host, 370 of 501 process entries
/// were unreadable and the rescan never matched. Treating that as a veto would make reclaim
/// permanently ineligible everywhere, which is why only a positive holder observation refuses.
/// These counts travel with the decision so an operator can see exactly how much the absence of
/// observed holders is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CargoTargetHolderEvidence {
    process_entries_started: u64,
    process_entries_completed: u64,
    process_entries_incomplete: u64,
    process_table_rescan_equal: bool,
    universal_absence_proven: bool,
}

impl CargoTargetHolderEvidence {
    #[must_use]
    pub const fn process_entries_incomplete(self) -> u64 {
        self.process_entries_incomplete
    }

    #[must_use]
    pub const fn universal_absence_proven(self) -> bool {
        self.universal_absence_proven
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetReclaimAuthority {
    /// The target is reconstructible from the checkout; losing it costs time, never truth.
    ReconstructibleFromCheckout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetReclaimVeto {
    /// No `target` directory was observed, so there is nothing to reclaim.
    TargetAbsent,
    /// The target is owned by a different user than the checkout that would rebuild it.
    TargetOwnerDiffersFromCheckout,
    /// At least one entry inside the target belongs to another user.
    ForeignEntriesPresent,
    /// Hardlinks reach outside the observed tree, so unlinking here need not free the bytes and
    /// can affect state this observation never examined.
    ExternalHardlinksPresent,
    /// No usable `.rustc_info.json`, so nothing establishes that Cargo produced this directory.
    NotCargoProduced,
    /// The newest entry is inside the policy's idle window.
    RecentlyModified,
    /// A process or mount namespace was observed referencing the target.
    HoldersObserved,
    /// Holder evidence describes a different physical target than the cost observation.
    HolderIdentityMismatch,
}

impl CargoTargetReclaimVeto {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TargetAbsent => "target_absent",
            Self::TargetOwnerDiffersFromCheckout => "target_owner_differs_from_checkout",
            Self::ForeignEntriesPresent => "foreign_entries_present",
            Self::ExternalHardlinksPresent => "external_hardlinks_present",
            Self::NotCargoProduced => "not_cargo_produced",
            Self::RecentlyModified => "recently_modified",
            Self::HoldersObserved => "holders_observed",
            Self::HolderIdentityMismatch => "holder_identity_mismatch",
        }
    }
}

impl fmt::Display for CargoTargetReclaimVeto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CargoTargetReclaimPolicy {
    minimum_idle_seconds: i64,
}

impl CargoTargetReclaimPolicy {
    /// Build a policy from an explicit idle window.
    ///
    /// # Errors
    ///
    /// Refuses a window below [`MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS`].
    pub const fn new(minimum_idle_seconds: i64) -> Result<Self, CargoTargetReclaimError> {
        if minimum_idle_seconds < MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS {
            return Err(idle_window_too_small());
        }
        Ok(Self {
            minimum_idle_seconds,
        })
    }

    #[must_use]
    pub const fn minimum_idle_seconds(self) -> i64 {
        self.minimum_idle_seconds
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CargoTargetReclaimDecision {
    Refused {
        vetoes: Vec<CargoTargetReclaimVeto>,
    },
    Eligible {
        authority: CargoTargetReclaimAuthority,
        allocated_bytes: u64,
        logical_bytes: u64,
        entry_count: u64,
        idle_seconds: i64,
        holder_evidence: CargoTargetHolderEvidence,
    },
}

impl CargoTargetReclaimDecision {
    #[must_use]
    pub const fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    #[must_use]
    pub fn vetoes(&self) -> &[CargoTargetReclaimVeto] {
        match self {
            Self::Refused { vetoes } => vetoes,
            Self::Eligible { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetReclaimPlan {
    schema_version: u8,
    decision: CargoTargetReclaimDecision,
    minimum_idle_seconds: i64,
}

impl CargoTargetReclaimPlan {
    #[must_use]
    pub const fn decision(&self) -> &CargoTargetReclaimDecision {
        &self.decision
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }
}

/// Decide whether one observed Cargo target may be reclaimed.
///
/// Every applicable veto is collected, so one refusal reports all of its reasons. `now_seconds` is
/// supplied rather than read, which keeps the decision a pure function of its observations.
///
/// # Errors
///
/// Refuses an observation whose newest entry is in the future relative to `now_seconds`, or whose
/// idle arithmetic would overflow. A future timestamp means the clock, the filesystem, or the
/// observation disagree, and no reclaim decision should be made from disagreeing evidence.
pub fn plan_cargo_target_reclaim(
    observation: &CargoTargetObservation,
    holders: &CargoTargetHolderObservation,
    policy: CargoTargetReclaimPolicy,
    now_seconds: i64,
) -> Result<CargoTargetReclaimPlan, CargoTargetReclaimError> {
    let mut vetoes = Vec::new();
    let mut evidence: Option<CargoTargetHolderEvidence> = None;

    let CargoTargetState::Present {
        target_id,
        entry_count,
        logical_bytes,
        allocated_bytes,
        latest_modified,
        target_owner_matches_checkout,
        all_entries_match_target_owner,
        hardlink_coverage,
        rustc_info,
        ..
    } = observation.state()
    else {
        return Ok(CargoTargetReclaimPlan {
            schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
            decision: CargoTargetReclaimDecision::Refused {
                vetoes: vec![CargoTargetReclaimVeto::TargetAbsent],
            },
            minimum_idle_seconds: policy.minimum_idle_seconds,
        });
    };

    if !target_owner_matches_checkout {
        vetoes.push(CargoTargetReclaimVeto::TargetOwnerDiffersFromCheckout);
    }
    if !all_entries_match_target_owner {
        vetoes.push(CargoTargetReclaimVeto::ForeignEntriesPresent);
    }
    if *hardlink_coverage == CargoTargetHardlinkCoverage::ExternalLinksPresent {
        vetoes.push(CargoTargetReclaimVeto::ExternalHardlinksPresent);
    }
    if !matches!(rustc_info, RustcInfoObservation::Observed { .. }) {
        vetoes.push(CargoTargetReclaimVeto::NotCargoProduced);
    }

    let idle_seconds = now_seconds
        .checked_sub(latest_modified.seconds())
        .ok_or_else(idle_overflow)?;
    if idle_seconds < 0 {
        return Err(future_timestamp());
    }
    if idle_seconds < policy.minimum_idle_seconds {
        vetoes.push(CargoTargetReclaimVeto::RecentlyModified);
    }

    match holders.state() {
        CargoTargetHolderState::Absent => {
            vetoes.push(CargoTargetReclaimVeto::HolderIdentityMismatch);
        }
        CargoTargetHolderState::Present {
            target_id: holder_target_id,
            disposition,
            coverage,
            ..
        } => {
            if holder_target_id != target_id {
                vetoes.push(CargoTargetReclaimVeto::HolderIdentityMismatch);
            }
            if *disposition == CargoTargetHolderDisposition::HoldersObserved {
                vetoes.push(CargoTargetReclaimVeto::HoldersObserved);
            }
            evidence = Some(CargoTargetHolderEvidence {
                process_entries_started: coverage.process_entries_started(),
                process_entries_completed: coverage.process_entries_completed(),
                process_entries_incomplete: coverage.process_entries_incomplete(),
                process_table_rescan_equal: coverage.process_table_rescan_equal(),
                universal_absence_proven: coverage.universal_absence_proven(),
            });
        }
    }

    vetoes.sort_unstable();
    vetoes.dedup();

    let decision = if vetoes.is_empty() {
        CargoTargetReclaimDecision::Eligible {
            authority: CargoTargetReclaimAuthority::ReconstructibleFromCheckout,
            allocated_bytes: *allocated_bytes,
            logical_bytes: *logical_bytes,
            entry_count: *entry_count,
            idle_seconds,
            holder_evidence: evidence.ok_or_else(missing_holder_evidence)?,
        }
    } else {
        CargoTargetReclaimDecision::Refused { vetoes }
    };

    Ok(CargoTargetReclaimPlan {
        schema_version: CARGO_TARGET_RECLAIM_SCHEMA_VERSION,
        decision,
        minimum_idle_seconds: policy.minimum_idle_seconds,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetReclaimErrorKind {
    InvalidPolicy,
    DisagreeingEvidence,
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetReclaimError {
    kind: CargoTargetReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl CargoTargetReclaimError {
    #[must_use]
    pub const fn kind(&self) -> CargoTargetReclaimErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for CargoTargetReclaimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for CargoTargetReclaimError {}

const fn error(
    kind: CargoTargetReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
) -> CargoTargetReclaimError {
    CargoTargetReclaimError {
        kind,
        code,
        problem,
    }
}

const fn idle_window_too_small() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::InvalidPolicy,
        "idle_window_too_small",
        "the reclaim idle window is below the reviewed minimum",
    )
}

const fn future_timestamp() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "future_timestamp",
        "the observed target is newer than the supplied clock reading",
    )
}

const fn missing_holder_evidence() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::DisagreeingEvidence,
        "missing_holder_evidence",
        "the target was observed but holder evidence was absent",
    )
}

const fn idle_overflow() -> CargoTargetReclaimError {
    error(
        CargoTargetReclaimErrorKind::Overflow,
        "idle_overflow",
        "the idle window could not be computed without overflow",
    )
}
