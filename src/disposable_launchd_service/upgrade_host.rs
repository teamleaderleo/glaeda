//! Production wiring for journaled canary service upgrades.
//!
//! Binds the approved [`CanaryUpgradePlan`] to verified service surfaces: preflight reobserves
//! the exact predecessor through an injected status observer (production: the launchd status
//! inspector), effects apply through an injected apply observer (production: the exact-plan
//! service adapter), and scheduler drain plus reviewed health-profile evaluation arrive as
//! injected bounded observers. Staged fleet-generation verification (`fleet_bundle.py inspect`)
//! stays an operator-side check per `docs/FLEET_DISTRIBUTION.md`; the drain source of truth and
//! the health-profile registry are decided under issue #149. Every refusal is fail-closed with a
//! fixed public code.

use std::time::Duration;

use super::DisposableLaunchdServicePlan;
#[cfg(target_os = "macos")]
use super::upgrade::apply_upgrade_service_plan;
use super::upgrade::{CanaryUpgradeHost, CanaryUpgradePlan, UpgradeHealthEvidence};
use crate::artifact::Sha256Digest;
use crate::disposable_launchd_service_status::inspect_disposable_launchd_service_status;
use crate::journal::ActionFailure;
use crate::process::TimedCommandExecutor;

/// Exact predecessor evidence: the plan identity the live service currently reports.
pub struct ServiceStatusEvidence {
    pub plan_identity: Sha256Digest,
}

/// Host boundary for predecessor observation. Production uses the launchd status inspector;
/// tests substitute a fake. Implementations perform no mutation.
pub trait ServiceStatusObserver {
    fn observe_status(
        &mut self,
        plan: &DisposableLaunchdServicePlan,
    ) -> Result<ServiceStatusEvidence, ActionFailure>;
}

/// Scheduler drain evidence: no admitted work may remain on the previous service.
pub struct DrainEvidence {
    pub no_active_work: bool,
}

/// Host boundary for drain observation. The drain source of truth is decided under issue #149;
/// production observers must reobserve zero active/reserved work immediately before each effect.
pub trait SchedulerDrainObserver {
    fn observe_drain(&mut self, plan: &CanaryUpgradePlan) -> Result<DrainEvidence, ActionFailure>;
}

/// Host boundary for reviewed health-profile evaluation. Implementations return only the result
/// of the bound reviewed profile within its timeout.
pub trait UpgradeHealthObserver {
    fn observe_health(
        &mut self,
        service: &DisposableLaunchdServicePlan,
        profile: &Sha256Digest,
        timeout: Duration,
    ) -> Result<UpgradeHealthEvidence, ActionFailure>;
}

/// Host boundary for service effects. Production applies through the exact-plan service adapter.
pub trait ServiceApplyObserver {
    fn observe_apply(&mut self, plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure>;
}

/// Production [`CanaryUpgradeHost`] composing injected bounded observers. Drain and health
/// sources are the operator's responsibility; predecessor and (on macOS) effects are wired to
/// the verified service surfaces by the constructors below.
pub struct ProductionUpgradeHost<S, D, H, A> {
    status: S,
    drain: D,
    health: H,
    apply: A,
}

impl<S, D, H, A> ProductionUpgradeHost<S, D, H, A> {
    /// Compose all four observers explicitly. Prefer [`ProductionUpgradeHost::macos`] on macOS.
    pub fn new(status: S, drain: D, health: H, apply: A) -> Self {
        Self {
            status,
            drain,
            health,
            apply,
        }
    }
}

/// Status observer backed by the launchd status inspector. Fails closed off macOS and whenever
/// the operator identity, filesystem evidence, or observation command cannot be proven exactly.
pub struct InspectorStatusObserver<E> {
    operator_home: std::path::PathBuf,
    executor: E,
}

impl<E> InspectorStatusObserver<E> {
    /// Bind the operator home the inspected plans must live under.
    pub fn new(operator_home: std::path::PathBuf, executor: E) -> Self {
        Self {
            operator_home,
            executor,
        }
    }
}

impl<E: TimedCommandExecutor> ServiceStatusObserver for InspectorStatusObserver<E> {
    fn observe_status(
        &mut self,
        plan: &DisposableLaunchdServicePlan,
    ) -> Result<ServiceStatusEvidence, ActionFailure> {
        let report = inspect_disposable_launchd_service_status(
            plan.operator_uid,
            &self.operator_home,
            &plan.program,
            &plan.program_digest,
            &plan.enrollment,
            &plan.enrollment_digest,
            &self.executor,
        )
        .map_err(|_| {
            ActionFailure::public(
                "upgrade_status_unavailable",
                "predecessor service status is unavailable",
            )
        })?;
        Ok(ServiceStatusEvidence {
            plan_identity: report.plan_identity().clone(),
        })
    }
}

/// Apply observer backed by the exact-plan service adapter. The adapter itself is macOS-only;
/// off macOS every effect is refused before touching service state.
pub struct AdapterApplyObserver<E> {
    executor: E,
}

impl<E> AdapterApplyObserver<E> {
    /// Bind the bounded command executor the service adapter settles through.
    pub fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: TimedCommandExecutor> ServiceApplyObserver for AdapterApplyObserver<E> {
    #[cfg(target_os = "macos")]
    fn observe_apply(&mut self, plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure> {
        apply_upgrade_service_plan(plan, &self.executor)
    }

    #[cfg(not(target_os = "macos"))]
    fn observe_apply(&mut self, _plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure> {
        Err(ActionFailure::public(
            "upgrade_apply_unsupported",
            "service effects require macOS",
        ))
    }
}

fn refused(code: &str, message: &str) -> ActionFailure {
    ActionFailure::public(code, message)
}

impl<S, D, H, A> CanaryUpgradeHost for ProductionUpgradeHost<S, D, H, A>
where
    S: ServiceStatusObserver,
    D: SchedulerDrainObserver,
    H: UpgradeHealthObserver,
    A: ServiceApplyObserver,
{
    fn preflight(&mut self, plan: &CanaryUpgradePlan) -> Result<(), ActionFailure> {
        let evidence = self.status.observe_status(plan.previous())?;
        if evidence.plan_identity != *plan.previous().report().plan_identity() {
            return Err(refused(
                "upgrade_predecessor_mismatch",
                "live service does not match the approved predecessor",
            ));
        }
        Ok(())
    }

    fn before_change(&mut self, plan: &CanaryUpgradePlan) -> Result<(), ActionFailure> {
        let evidence = self.drain.observe_drain(plan)?;
        if !evidence.no_active_work {
            return Err(refused(
                "upgrade_drain_occupied",
                "scheduler drain reports active work",
            ));
        }
        Ok(())
    }

    fn apply(&mut self, plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure> {
        self.apply.observe_apply(plan)
    }

    fn health(
        &mut self,
        service: &DisposableLaunchdServicePlan,
        profile: &Sha256Digest,
        timeout: Duration,
    ) -> Result<UpgradeHealthEvidence, ActionFailure> {
        let start = std::time::Instant::now();
        let evidence = self.health.observe_health(service, profile, timeout)?;
        if start.elapsed() > timeout
            || !evidence.healthy
            || evidence.service_plan != *service.report().plan_identity()
            || evidence.health_profile != *profile
        {
            return Err(refused(
                "upgrade_health_failed",
                "bound health/canary check failed",
            ));
        }
        Ok(evidence)
    }
}
