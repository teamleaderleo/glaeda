from pathlib import Path

path = Path("src/mac_availability.rs")
text = path.read_text()

anchor = '''    if !effective_state_is_consistent(observation.effective_mode, observation.vm_power) {
        blockers.push(blocker(
            AvailabilityBlockerKind::InconsistentEffectiveState,
            "the effective availability mode conflicts with the observed VM power state",
        ));
    }

'''
replacement = anchor + '''    classify_off_job_consistency(observation, &mut blockers);

'''
if text.count(anchor) != 1:
    raise SystemExit("effective-state anchor missing or duplicated")
text = text.replace(anchor, replacement, 1)

function_anchor = '''fn classify_start_capacity(
'''
helper = '''fn classify_off_job_consistency(
    observation: MacAvailabilityObservation,
    blockers: &mut Vec<AvailabilityBlocker>,
) {
    if observation.effective_mode != EffectiveAvailabilityMode::Off {
        return;
    }
    match observation.job_activity {
        JobActivity::Idle => {}
        JobActivity::Active => blockers.push(blocker(
            AvailabilityBlockerKind::InconsistentEffectiveState,
            "the VM is off but runner job activity is reported as active",
        )),
        JobActivity::Unknown => blockers.push(blocker(
            AvailabilityBlockerKind::UnknownJobActivity,
            "the VM is off but runner job activity is unknown; prove the listener idle",
        )),
    }
}

'''
if text.count(function_anchor) != 1:
    raise SystemExit("capacity anchor missing or duplicated")
text = text.replace(function_anchor, helper + function_anchor, 1)

old_barrier = '''    if request_matches_effective(requested_mode, observation.effective_mode) {
        return;
    }
'''
new_barrier = '''    if observation.effective_mode == EffectiveAvailabilityMode::Off
        || request_matches_effective(requested_mode, observation.effective_mode)
    {
        return;
    }
'''
if text.count(old_barrier) != 1:
    raise SystemExit("job-barrier anchor missing or duplicated")
text = text.replace(old_barrier, new_barrier, 1)

test_anchor = '''    #[test]
    fn active_to_away_requires_drain_restart_and_verification() {
'''
tests = '''    #[test]
    fn off_idle_matching_request_is_no_change() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Off),
            AvailabilityRequest::Off,
        );

        assert_eq!(plan.disposition, AvailabilityDisposition::NoChange);
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn off_active_job_is_inconsistent_and_blocked() {
        let mut facts = observation(EffectiveAvailabilityMode::Off);
        facts.job_activity = JobActivity::Active;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Off);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(plan.blockers.iter().any(|blocker| {
            blocker.kind == AvailabilityBlockerKind::InconsistentEffectiveState
        }));
    }

    #[test]
    fn off_unknown_job_activity_is_explicitly_blocked() {
        let mut facts = observation(EffectiveAvailabilityMode::Off);
        facts.job_activity = JobActivity::Unknown;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Off);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(plan
            .blockers
            .iter()
            .any(|blocker| blocker.kind == AvailabilityBlockerKind::UnknownJobActivity));
    }

    #[test]
    fn matching_running_mode_allows_an_active_job() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.job_activity = JobActivity::Active;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Active);

        assert_eq!(plan.disposition, AvailabilityDisposition::NoChange);
        assert!(plan.blockers.is_empty());
    }

'''
if text.count(test_anchor) != 1:
    raise SystemExit("test anchor missing or duplicated")
text = text.replace(test_anchor, tests + test_anchor, 1)
path.write_text(text)
