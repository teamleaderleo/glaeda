#![cfg(target_os = "linux")]

//! Acceptance tests for Cargo target reclaim planning.
//!
//! These drive real directories through the real observers, so they check the decision an operator
//! would actually get on a Linux host rather than a hand-built observation.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::cargo_target_holder_observation::observe_cargo_target_holders;
use glaeda::cargo_target_observation::{observe_cargo_target, CargoTargetState};
use glaeda::cargo_target_reclaim::{
    plan_cargo_target_reclaim, CargoTargetReclaimDecision, CargoTargetReclaimError,
    CargoTargetReclaimPolicy, CargoTargetReclaimVeto, MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

const ONE_DAY: i64 = 86_400;

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glaeda-cargo-target-reclaim-{}-{sequence}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).expect("create fixture checkout");
        Self { root, checkout }
    }

    /// A target directory shaped the way Cargo leaves one.
    fn with_cargo_target(self) -> Self {
        let target = self.checkout.join("target");
        fs::create_dir_all(target.join("debug")).expect("create target/debug");
        let mut info = File::create(target.join(".rustc_info.json")).expect("create rustc info");
        info.write_all(br#"{"rustc_fingerprint":1}"#)
            .expect("write rustc info");
        let mut artifact = File::create(target.join("debug").join("libfixture.rlib"))
            .expect("create artifact");
        artifact.write_all(&[0_u8; 4096]).expect("write artifact");
        self
    }

    /// A directory named `target` that Cargo did not produce.
    fn with_foreign_target(self) -> Self {
        let target = self.checkout.join("target");
        fs::create_dir_all(&target).expect("create target");
        let mut stray = File::create(target.join("notes.txt")).expect("create stray file");
        stray.write_all(b"not cargo output").expect("write stray");
        self
    }

    fn newest_entry_seconds(&self) -> i64 {
        let observation = observe_cargo_target(&self.checkout).expect("observe target");
        match observation.state() {
            CargoTargetState::Present {
                latest_modified, ..
            } => latest_modified.seconds(),
            CargoTargetState::Absent => panic!("fixture target is absent"),
        }
    }

    fn plan_at(
        &self,
        now_seconds: i64,
    ) -> Result<glaeda::cargo_target_reclaim::CargoTargetReclaimPlan, CargoTargetReclaimError> {
        let observation = observe_cargo_target(&self.checkout).expect("observe target");
        let holders = observe_cargo_target_holders(&self.checkout).expect("observe holders");
        let policy = CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS)
            .expect("reviewed policy");
        plan_cargo_target_reclaim(&observation, &holders, policy, now_seconds)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn an_idle_cargo_produced_target_is_eligible() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let plan = fixture.plan_at(now).expect("plan");

    match plan.decision() {
        CargoTargetReclaimDecision::Eligible {
            allocated_bytes,
            entry_count,
            idle_seconds,
            ..
        } => {
            assert!(*allocated_bytes > 0, "the fixture wrote a real artifact");
            assert!(*entry_count > 0, "the fixture wrote real entries");
            assert_eq!(*idle_seconds, ONE_DAY);
        }
        CargoTargetReclaimDecision::Refused { vetoes } => {
            panic!("an idle Cargo target should be eligible, refused by {vetoes:?}")
        }
    }
}

#[test]
fn incomplete_holder_coverage_does_not_refuse() {
    // The regression this guards: `/proc` coverage is never complete on an ordinary host, so
    // requiring completeness made every target permanently ineligible. Assert both halves -- that
    // coverage really is incomplete here, and that the decision is still eligible -- so the test
    // fails if either the host stops being representative or the veto comes back.
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let plan = fixture.plan_at(now).expect("plan");

    let CargoTargetReclaimDecision::Eligible {
        holder_evidence, ..
    } = plan.decision()
    else {
        panic!("incomplete holder coverage must not refuse: {:?}", plan.decision());
    };
    assert!(
        holder_evidence.process_entries_incomplete() > 0,
        "this host was expected to have unreadable /proc entries, so the case is exercised"
    );
    assert!(
        !holder_evidence.universal_absence_proven(),
        "positive-only observation never proves universal absence"
    );
}

#[test]
fn a_recently_modified_target_is_refused() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + 60;

    let plan = fixture.plan_at(now).expect("plan");

    assert!(
        plan.decision()
            .vetoes()
            .contains(&CargoTargetReclaimVeto::RecentlyModified),
        "expected RecentlyModified, got {:?}",
        plan.decision()
    );
}

#[test]
fn a_directory_cargo_did_not_produce_is_refused() {
    let fixture = Fixture::new().with_foreign_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let plan = fixture.plan_at(now).expect("plan");

    assert!(
        plan.decision()
            .vetoes()
            .contains(&CargoTargetReclaimVeto::NotCargoProduced),
        "expected NotCargoProduced, got {:?}",
        plan.decision()
    );
}

#[test]
fn an_absent_target_is_refused() {
    let fixture = Fixture::new();
    let observation = observe_cargo_target(&fixture.checkout).expect("observe target");
    let holders = observe_cargo_target_holders(&fixture.checkout).expect("observe holders");
    let policy =
        CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS).expect("policy");

    let plan = plan_cargo_target_reclaim(&observation, &holders, policy, 0).expect("plan");

    assert_eq!(
        plan.decision().vetoes(),
        &[CargoTargetReclaimVeto::TargetAbsent]
    );
}

#[test]
fn an_idle_window_below_the_reviewed_minimum_is_refused() {
    let error = CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS - 1)
        .expect_err("a sub-minimum idle window must not build a policy");
    assert_eq!(error.code(), "idle_window_too_small");
}

#[test]
fn a_target_newer_than_the_clock_is_disagreeing_evidence() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() - 1;

    let error = fixture
        .plan_at(now)
        .expect_err("a future target must not produce a decision");

    assert_eq!(error.code(), "future_timestamp");
}
