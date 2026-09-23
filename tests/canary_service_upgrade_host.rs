use std::path::Path;
use std::time::Duration;

use glaeda::artifact::Sha256Digest;
use glaeda::disposable_launchd_service::upgrade::{
    CanaryUpgradeError, CanaryUpgradePlan, UpgradeHealthEvidence,
};
use glaeda::disposable_launchd_service::upgrade_host::{
    DrainEvidence, ProductionUpgradeHost, SchedulerDrainObserver, ServiceApplyObserver,
    ServiceStatusEvidence, ServiceStatusObserver, UpgradeHealthObserver,
};
use glaeda::disposable_launchd_service::{
    DisposableLaunchdServiceDesiredState as Desired, DisposableLaunchdServicePlan,
    plan_disposable_launchd_service,
};
use glaeda::durable_journal::{JournalCheckpoint, JournalCheckpointFailure};
use glaeda::journal::{ActionFailure, ActionOutcome, ExecutionJournal};

fn digest(c: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", c.to_string().repeat(64))).unwrap()
}
fn service(candidate: bool) -> DisposableLaunchdServicePlan {
    plan_disposable_launchd_service(
        Desired::Installed,
        501,
        Path::new("/Users/operator"),
        Path::new(if candidate {
            "/private/candidate/glaeda"
        } else {
            "/private/previous/glaeda"
        }),
        &digest(if candidate { 'b' } else { 'a' }),
        Path::new(if candidate {
            "/private/candidate/enrollment"
        } else {
            "/private/previous/enrollment"
        }),
        &digest('c'),
    )
    .unwrap()
}
fn plan() -> CanaryUpgradePlan {
    CanaryUpgradePlan::new(
        service(false),
        service(true),
        digest('d'),
        Duration::from_secs(30),
    )
    .unwrap()
}
fn failure() -> ActionFailure {
    ActionFailure::public("fixture_failure", "fixture failure")
}

struct Status {
    identity: Option<Sha256Digest>,
    fail: bool,
}
impl Status {
    fn matching() -> Self {
        Self {
            identity: Some(service(false).report().plan_identity().clone()),
            fail: false,
        }
    }
}
impl ServiceStatusObserver for Status {
    fn observe_status(
        &mut self,
        _: &DisposableLaunchdServicePlan,
    ) -> Result<ServiceStatusEvidence, ActionFailure> {
        if self.fail {
            return Err(failure());
        }
        Ok(ServiceStatusEvidence {
            plan_identity: self.identity.clone().unwrap_or_else(|| digest('f')),
        })
    }
}

struct Drain {
    empty: bool,
    fail: bool,
    calls: usize,
}
impl Drain {
    fn empty() -> Self {
        Self {
            empty: true,
            fail: false,
            calls: 0,
        }
    }
}
impl SchedulerDrainObserver for Drain {
    fn observe_drain(&mut self, _: &CanaryUpgradePlan) -> Result<DrainEvidence, ActionFailure> {
        self.calls += 1;
        if self.fail {
            return Err(failure());
        }
        Ok(DrainEvidence {
            no_active_work: self.empty,
        })
    }
}

struct Health {
    candidate_healthy: bool,
    previous_healthy: bool,
    wrong_identity: bool,
    wrong_profile: bool,
}
impl Health {
    fn healthy() -> Self {
        Self {
            candidate_healthy: true,
            previous_healthy: true,
            wrong_identity: false,
            wrong_profile: false,
        }
    }
}
impl UpgradeHealthObserver for Health {
    fn observe_health(
        &mut self,
        svc: &DisposableLaunchdServicePlan,
        profile: &Sha256Digest,
        _: Duration,
    ) -> Result<UpgradeHealthEvidence, ActionFailure> {
        let candidate = svc.report().plan_identity() == service(true).report().plan_identity();
        Ok(UpgradeHealthEvidence {
            service_plan: if candidate && self.wrong_identity {
                digest('f')
            } else {
                svc.report().plan_identity().clone()
            },
            health_profile: if candidate && self.wrong_profile {
                digest('f')
            } else {
                profile.clone()
            },
            healthy: if candidate {
                self.candidate_healthy
            } else {
                self.previous_healthy
            },
        })
    }
}

#[derive(Default)]
struct Apply {
    events: Vec<String>,
    fail_candidate_install: bool,
}
impl ServiceApplyObserver for Apply {
    fn observe_apply(&mut self, plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure> {
        let candidate = plan.report().plan_identity() == service(true).report().plan_identity();
        let candidate_remove = plan_disposable_launchd_service(
            Desired::Removed,
            501,
            Path::new("/Users/operator"),
            Path::new("/private/candidate/glaeda"),
            &digest('b'),
            Path::new("/private/candidate/enrollment"),
            &digest('c'),
        )
        .unwrap();
        let is_candidate_remove =
            plan.report().plan_identity() == candidate_remove.report().plan_identity();
        if plan.report().desired_state() == Desired::Installed {
            self.events.push(
                if candidate {
                    "start_candidate"
                } else {
                    "restore_previous"
                }
                .into(),
            );
            if candidate && self.fail_candidate_install {
                return Err(failure());
            }
        } else if is_candidate_remove {
            self.events.push("remove_candidate".into());
        } else {
            self.events.push("stop_previous".into());
        }
        Ok(())
    }
}

#[derive(Default)]
struct Checkpoint {
    snapshots: Vec<ExecutionJournal>,
}
impl JournalCheckpoint for Checkpoint {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure> {
        self.snapshots.push(journal.clone());
        Ok(())
    }
}

fn host(
    status: Status,
    drain: Drain,
    health: Health,
    apply: Apply,
) -> ProductionUpgradeHost<Status, Drain, Health, Apply> {
    ProductionUpgradeHost::new(status, drain, health, apply)
}

#[test]
fn healthy_production_path_completes() {
    let p = plan();
    let mut h = host(
        Status::matching(),
        Drain::empty(),
        Health::healthy(),
        Apply::default(),
    );
    let mut c = Checkpoint::default();
    let journal = glaeda::disposable_launchd_service::upgrade::execute_canary_upgrade(
        &p,
        p.identity(),
        &mut h,
        &mut c,
    )
    .unwrap();
    assert!(
        journal
            .records
            .iter()
            .all(|r| r.outcome == ActionOutcome::Completed)
    );
}

#[test]
fn predecessor_mismatch_refuses_before_any_effect() {
    let p = plan();
    let mut h = host(
        Status {
            identity: None,
            fail: false,
        },
        Drain::empty(),
        Health::healthy(),
        Apply::default(),
    );
    let mut c = Checkpoint::default();
    assert!(matches!(
        glaeda::disposable_launchd_service::upgrade::execute_canary_upgrade(
            &p,
            p.identity(),
            &mut h,
            &mut c
        ),
        Err(CanaryUpgradeError::PreflightRefused)
    ));
}

#[test]
fn status_unavailable_refuses_before_any_effect() {
    let p = plan();
    let mut h = host(
        Status {
            identity: Some(service(false).report().plan_identity().clone()),
            fail: true,
        },
        Drain::empty(),
        Health::healthy(),
        Apply::default(),
    );
    let mut c = Checkpoint::default();
    assert!(matches!(
        glaeda::disposable_launchd_service::upgrade::execute_canary_upgrade(
            &p,
            p.identity(),
            &mut h,
            &mut c
        ),
        Err(CanaryUpgradeError::PreflightRefused)
    ));
}

#[test]
fn occupied_drain_stops_before_first_effect() {
    let p = plan();
    let mut h = host(
        Status::matching(),
        Drain {
            empty: false,
            fail: false,
            calls: 0,
        },
        Health::healthy(),
        Apply::default(),
    );
    let mut c = Checkpoint::default();
    let journal = glaeda::disposable_launchd_service::upgrade::execute_canary_upgrade(
        &p,
        p.identity(),
        &mut h,
        &mut c,
    )
    .unwrap();
    assert_eq!(journal.records[0].outcome, ActionOutcome::Failed);
}

#[test]
fn unhealthy_candidate_restores_previous_through_production_host() {
    let p = plan();
    let mut h = host(
        Status::matching(),
        Drain::empty(),
        Health {
            candidate_healthy: false,
            previous_healthy: true,
            wrong_identity: false,
            wrong_profile: false,
        },
        Apply::default(),
    );
    let mut c = Checkpoint::default();
    let journal = glaeda::disposable_launchd_service::upgrade::execute_canary_upgrade(
        &p,
        p.identity(),
        &mut h,
        &mut c,
    )
    .unwrap();
    assert_eq!(journal.records[0].outcome, ActionOutcome::Compensated);
    assert_eq!(journal.records[1].outcome, ActionOutcome::Compensated);
    assert_eq!(journal.records[2].outcome, ActionOutcome::Failed);
}

#[test]
fn misbound_health_evidence_restores_previous() {
    for wrong_profile in [true, false] {
        let p = plan();
        let mut h = host(
            Status::matching(),
            Drain::empty(),
            Health {
                candidate_healthy: true,
                previous_healthy: true,
                wrong_identity: !wrong_profile,
                wrong_profile,
            },
            Apply::default(),
        );
        let mut c = Checkpoint::default();
        let journal = glaeda::disposable_launchd_service::upgrade::execute_canary_upgrade(
            &p,
            p.identity(),
            &mut h,
            &mut c,
        )
        .unwrap();
        assert_eq!(journal.records[2].outcome, ActionOutcome::Failed);
    }
}
