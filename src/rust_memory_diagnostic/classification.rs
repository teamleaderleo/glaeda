
/// Classify one Rust verification attempt from already-trusted bounded observations.
///
/// This function performs no I/O, command execution, cgroup read, clock read, retry choice, or
/// state mutation.
///
/// # Errors
///
/// Returns a bounded error for identity drift, stale/future/contradictory timing, partial evidence,
/// counter reversal/overflow, process-group limit drift, or incompatible OOM and terminal evidence.
pub fn classify_rust_memory(
    input: RustMemoryDiagnosticInput,
) -> Result<RustMemoryDiagnosticReport, RustMemoryDiagnosticError> {
    validate_binding("preflight.binding", &input.binding.identity, &input.preflight.binding)?;
    validate_binding("terminal.binding", &input.binding.identity, &input.terminal.binding)?;
    validate_not_future(
        "preflight.observed_at",
        input.preflight.observed_at,
        input.timing.classified_at,
    )?;
    validate_not_future(
        "terminal.observed_at",
        input.terminal.observed_at,
        input.timing.classified_at,
    )?;

    let headroom_sufficient = input.preflight.available_memory_bytes
        >= input.binding.authority.minimum_guest_available_memory_bytes
        && input.preflight.available_swap_bytes
            >= input.binding.authority.minimum_guest_available_swap_bytes;

    if !headroom_sufficient {
        validate_refused_evidence(&input)?;
        return Ok(report(
            input,
            RustMemoryDiagnosticClassification::MemoryPressureRefused,
            RustCgroupMemorySummary::NotCreated,
        ));
    }

    let started_at = input.timing.attempt_started_at.ok_or_else(|| {
        RustMemoryDiagnosticError::new(
            "timing.attempt_started_at",
            "missing_attempt_start",
            "sufficient preflight followed by terminal execution evidence requires an attempt start",
        )
    })?;
    validate_executed_timing(&input, started_at)?;

    let (cgroup_summary, event_delta) = validate_cgroup_evidence(&input, started_at)?;
    let classification = classify_terminal(
        input.terminal.phase,
        input.terminal.termination,
        event_delta,
    )?;

    Ok(report(input, classification, cgroup_summary))
}

fn validate_refused_evidence(input: &RustMemoryDiagnosticInput) -> Result<(), RustMemoryDiagnosticError> {
    if input.timing.attempt_started_at.is_some()
        || input.terminal.phase != RustExecutionPhase::NotStarted
        || input.terminal.termination != RustProcessTermination::NotStarted
        || !matches!(&input.cgroup, RustCgroupMemoryEvidence::NotCreated)
    {
        return Err(RustMemoryDiagnosticError::new(
            "preflight",
            "preflight_execution_contradiction",
            "insufficient headroom may only classify an attempt that never started or created its process group",
        ));
    }
    let age = input
        .timing
        .classified_at
        .get()
        .checked_sub(input.preflight.observed_at.get())
        .ok_or_else(|| future_error("preflight.observed_at"))?;
    if age > input.timing.maximum_preflight_to_start_millis {
        return Err(stale_error("preflight.observed_at"));
    }
    let terminal_age = input
        .timing
        .classified_at
        .get()
        .checked_sub(input.terminal.observed_at.get())
        .ok_or_else(|| future_error("terminal.observed_at"))?;
    if terminal_age > input.timing.maximum_terminal_to_classification_millis {
        return Err(stale_error("terminal.observed_at"));
    }
    Ok(())
}

fn validate_executed_timing(
    input: &RustMemoryDiagnosticInput,
    started_at: EpochMillis,
) -> Result<(), RustMemoryDiagnosticError> {
    if input.terminal.phase == RustExecutionPhase::NotStarted
        || input.terminal.termination == RustProcessTermination::NotStarted
    {
        return Err(RustMemoryDiagnosticError::new(
            "terminal",
            "missing_terminal_execution_evidence",
            "a started attempt requires one non-empty typed terminal observation",
        ));
    }
    if input.preflight.observed_at > started_at {
        return Err(RustMemoryDiagnosticError::new(
            "preflight.observed_at",
            "preflight_after_attempt_start",
            "preflight evidence must precede the exact attempt start",
        ));
    }
    let preflight_lag = started_at
        .get()
        .checked_sub(input.preflight.observed_at.get())
        .ok_or_else(|| future_error("preflight.observed_at"))?;
    if preflight_lag > input.timing.maximum_preflight_to_start_millis {
        return Err(stale_error("preflight.observed_at"));
    }
    if input.terminal.observed_at < started_at {
        return Err(RustMemoryDiagnosticError::new(
            "terminal.observed_at",
            "terminal_before_attempt_start",
            "terminal evidence may not precede the exact attempt start",
        ));
    }
    let terminal_age = input
        .timing
        .classified_at
        .get()
        .checked_sub(input.terminal.observed_at.get())
        .ok_or_else(|| future_error("terminal.observed_at"))?;
    if terminal_age > input.timing.maximum_terminal_to_classification_millis {
        return Err(stale_error("terminal.observed_at"));
    }
    let elapsed = input
        .terminal
        .observed_at
        .get()
        .checked_sub(started_at.get())
        .ok_or_else(|| future_error("terminal.observed_at"))?;
    match input.terminal.termination {
        RustProcessTermination::Timeout => {
            if elapsed < input.binding.authority.maximum_execution_millis {
                return Err(RustMemoryDiagnosticError::new(
                    "terminal.termination",
                    "premature_timeout",
                    "timeout evidence may not precede the reviewed maximum execution duration",
                ));
            }
        }
        _ if elapsed > input.binding.authority.maximum_execution_millis => {
            return Err(RustMemoryDiagnosticError::new(
                "terminal.observed_at",
                "execution_duration_exceeded",
                "non-timeout terminal evidence exceeded the reviewed maximum execution duration",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_cgroup_evidence(
    input: &RustMemoryDiagnosticInput,
    started_at: EpochMillis,
) -> Result<(RustCgroupMemorySummary, Option<RustMemoryEventDelta>), RustMemoryDiagnosticError> {
    match &input.cgroup {
        RustCgroupMemoryEvidence::NotCreated => Err(RustMemoryDiagnosticError::new(
            "cgroup",
            "missing_cgroup_evidence",
            "a started attempt requires complete evidence or typed runner-loss unavailability",
        )),
        RustCgroupMemoryEvidence::UnavailableAfterRunnerLoss { before } => {
            if input.terminal.termination != RustProcessTermination::RunnerLost {
                return Err(RustMemoryDiagnosticError::new(
                    "cgroup",
                    "partial_cgroup_evidence",
                    "after-observation unavailability is valid only for a typed runner loss",
                ));
            }
            validate_cgroup_before(input, before, started_at)?;
            Ok((
                RustCgroupMemorySummary::UnavailableAfterRunnerLoss {
                    memory_limit_bytes: before.memory_limit_bytes,
                    memory_peak_bytes: before.memory_peak_bytes,
                },
                None,
            ))
        }
        RustCgroupMemoryEvidence::Complete { before, after } => {
            validate_cgroup_before(input, before, started_at)?;
            validate_binding("cgroup.after.binding", &input.binding.identity, &after.binding)?;
            validate_not_future(
                "cgroup.after.observed_at",
                after.observed_at,
                input.timing.classified_at,
            )?;
            if after.observed_at < input.terminal.observed_at {
                return Err(RustMemoryDiagnosticError::new(
                    "cgroup.after.observed_at",
                    "cgroup_after_before_terminal",
                    "the final process-group observation must not precede terminal process evidence",
                ));
            }
            let after_age = input
                .timing
                .classified_at
                .get()
                .checked_sub(after.observed_at.get())
                .ok_or_else(|| future_error("cgroup.after.observed_at"))?;
            if after_age > input.timing.maximum_terminal_to_classification_millis {
                return Err(stale_error("cgroup.after.observed_at"));
            }
            if before.memory_limit_bytes != after.memory_limit_bytes
                || before.memory_limit_bytes != input.binding.authority.reserved_memory_bytes
            {
                return Err(RustMemoryDiagnosticError::new(
                    "cgroup.memory_limit_bytes",
                    "cgroup_limit_identity_drift",
                    "before/after process-group limits must equal the exact reserved memory authority",
                ));
            }
            if after.memory_peak_bytes < before.memory_peak_bytes {
                return Err(RustMemoryDiagnosticError::new(
                    "cgroup.memory_peak_bytes",
                    "memory_peak_reversal",
                    "process-group peak memory may not decrease within one generation",
                ));
            }
            let events = event_delta(before.events, after.events)?;
            Ok((
                RustCgroupMemorySummary::Complete {
                    memory_limit_bytes: after.memory_limit_bytes,
                    memory_peak_bytes: after.memory_peak_bytes,
                    events,
                },
                Some(events),
            ))
        }
    }
}

fn validate_cgroup_before(
    input: &RustMemoryDiagnosticInput,
    before: &RustCgroupMemoryObservation,
    started_at: EpochMillis,
) -> Result<(), RustMemoryDiagnosticError> {
    validate_binding("cgroup.before.binding", &input.binding.identity, &before.binding)?;
    validate_not_future(
        "cgroup.before.observed_at",
        before.observed_at,
        input.timing.classified_at,
    )?;
    if before.observed_at < input.preflight.observed_at || before.observed_at > started_at {
        return Err(RustMemoryDiagnosticError::new(
            "cgroup.before.observed_at",
            "invalid_cgroup_before_timing",
            "initial process-group evidence must follow preflight and not exceed attempt start",
        ));
    }
    if before.memory_limit_bytes != input.binding.authority.reserved_memory_bytes {
        return Err(RustMemoryDiagnosticError::new(
            "cgroup.memory_limit_bytes",
            "cgroup_limit_identity_drift",
            "process-group memory limit must equal the exact reserved memory authority",
        ));
    }
    Ok(())
}

fn event_delta(
    before: RustMemoryEventCounters,
    after: RustMemoryEventCounters,
) -> Result<RustMemoryEventDelta, RustMemoryDiagnosticError> {
    Ok(RustMemoryEventDelta {
        low: checked_counter_delta("cgroup.events.low", before.low, after.low)?,
        high: checked_counter_delta("cgroup.events.high", before.high, after.high)?,
        max: checked_counter_delta("cgroup.events.max", before.max, after.max)?,
        oom: checked_counter_delta("cgroup.events.oom", before.oom, after.oom)?,
        oom_kill: checked_counter_delta(
            "cgroup.events.oom_kill",
            before.oom_kill,
            after.oom_kill,
        )?,
    })
}

fn checked_counter_delta(
    field: &'static str,
    before: u64,
    after: u64,
) -> Result<u64, RustMemoryDiagnosticError> {
    after.checked_sub(before).ok_or_else(|| {
        RustMemoryDiagnosticError::new(
            field,
            "memory_event_counter_reversal",
            "memory-event counters may not decrease within one exact process-group generation",
        )
    })
}

fn classify_terminal(
    phase: RustExecutionPhase,
    termination: RustProcessTermination,
    events: Option<RustMemoryEventDelta>,
) -> Result<RustMemoryDiagnosticClassification, RustMemoryDiagnosticError> {
    let oom_kill_delta = events.map_or(0, |delta| delta.oom_kill);
    if oom_kill_delta > 0 {
        return match termination {
            RustProcessTermination::Signaled { signal } if signal.get() == 9 => {
                Ok(RustMemoryDiagnosticClassification::MemoryExhausted)
            }
            _ => Err(RustMemoryDiagnosticError::new(
                "terminal.termination",
                "oom_terminal_mismatch",
                "positive OOM-kill evidence requires compatible signal-9 terminal process evidence",
            )),
        };
    }

    match termination {
        RustProcessTermination::NotStarted => Err(RustMemoryDiagnosticError::new(
            "terminal.termination",
            "unexpected_not_started",
            "a started attempt may not end with not-started terminal evidence",
        )),
        RustProcessTermination::Timeout => Ok(RustMemoryDiagnosticClassification::Timeout),
        RustProcessTermination::RunnerLost => Ok(RustMemoryDiagnosticClassification::RunnerLost),
        RustProcessTermination::Signaled { signal } if signal.get() == 9 => {
            Ok(RustMemoryDiagnosticClassification::Inconclusive)
        }
        RustProcessTermination::Exited { code: 0 } => {
            if phase == RustExecutionPhase::Completed {
                Ok(RustMemoryDiagnosticClassification::Succeeded)
            } else {
                Err(RustMemoryDiagnosticError::new(
                    "terminal.phase",
                    "success_phase_contradiction",
                    "zero exit status requires the typed completed phase",
                ))
            }
        }
        RustProcessTermination::Exited { .. } | RustProcessTermination::Signaled { .. } => {
            Ok(match phase {
                RustExecutionPhase::Compile => RustMemoryDiagnosticClassification::CompileFailed,
                RustExecutionPhase::Link => RustMemoryDiagnosticClassification::LinkFailed,
                RustExecutionPhase::Test => RustMemoryDiagnosticClassification::TestFailed,
                RustExecutionPhase::Cleanup
                | RustExecutionPhase::Completed
                | RustExecutionPhase::Unknown => RustMemoryDiagnosticClassification::Inconclusive,
                RustExecutionPhase::NotStarted => {
                    return Err(RustMemoryDiagnosticError::new(
                        "terminal.phase",
                        "failure_phase_contradiction",
                        "failure terminal evidence may not use the not-started phase",
                    ));
                }
            })
        }
    }
}

fn report(
    input: RustMemoryDiagnosticInput,
    classification: RustMemoryDiagnosticClassification,
    cgroup: RustCgroupMemorySummary,
) -> RustMemoryDiagnosticReport {
    RustMemoryDiagnosticReport {
        schema_version: RUST_MEMORY_DIAGNOSTIC_SCHEMA_VERSION,
        identity: input.binding.identity,
        authority: input.binding.authority,
        classification,
        phase: input.terminal.phase,
        termination: input.terminal.termination,
        preflight_available_memory_bytes: input.preflight.available_memory_bytes,
        preflight_available_swap_bytes: input.preflight.available_swap_bytes,
        cgroup,
        classified_at: input.timing.classified_at,
    }
}

