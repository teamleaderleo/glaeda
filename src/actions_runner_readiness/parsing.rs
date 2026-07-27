fn classify_state(
    processes: &ProcessSnapshot,
    draining: bool,
) -> Result<ActionsRunnerReadinessState, ObservationProblem> {
    match (
        draining,
        processes.listener.is_some(),
        processes.worker.is_some(),
    ) {
        (true, true, _) => Ok(ActionsRunnerReadinessState::Draining),
        (false, false, false) => Ok(ActionsRunnerReadinessState::Starting),
        (false, true, false) => Ok(ActionsRunnerReadinessState::IdleReady),
        (false, true, true) => Ok(ActionsRunnerReadinessState::Busy),
        _ => Err(ObservationProblem::new(
            ActionsRunnerReadinessRefusalCode::ProcessStateInconsistent,
            ActionsRunnerReadinessPhase::FinalObservation,
            "the official runner listener, worker, and drain evidence are inconsistent",
        )),
    }
}

fn report(
    request: &ActionsRunnerReadinessRequest,
    state: ActionsRunnerReadinessState,
    configured_identity: Option<ActionsRunnerConfiguredIdentity>,
    timing: LimaObservationTiming,
) -> ActionsRunnerReadinessReport {
    ActionsRunnerReadinessReport {
        schema_version: ACTIONS_RUNNER_READINESS_SCHEMA_VERSION,
        instance: request.instance.clone(),
        runner_name: request.runner_name.clone(),
        state,
        configured_identity,
        timing,
    }
}

fn timing(
    started_at: u64,
    observed_at: u64,
    source: &LimaInstanceObservationReport,
    freshness: LimaObservationFreshness,
) -> Result<LimaObservationTiming, ObservationProblem> {
    let duration_seconds = observed_at
        .checked_sub(started_at)
        .ok_or_else(clock_problem)?;
    Ok(LimaObservationTiming {
        started_at_unix_seconds: started_at,
        observed_at_unix_seconds: observed_at,
        expires_at_unix_seconds: source.timing.expires_at_unix_seconds,
        duration_seconds,
        freshness,
    })
}

fn parse_pid_lines(
    output: &str,
    phase: ActionsRunnerReadinessPhase,
) -> Result<Vec<u32>, ObservationProblem> {
    let Some(body) = output.strip_suffix('\n') else {
        return Err(malformed_identity(
            phase,
            "the official runner PID evidence is not newline terminated",
        ));
    };
    if body.is_empty() || body.contains('\r') {
        return Err(malformed_identity(
            phase,
            "the official runner PID evidence is malformed",
        ));
    }
    let mut pids = Vec::new();
    for line in body.split('\n') {
        if pids.len() >= MAX_PROCESS_MATCHES {
            return Err(malformed_identity(
                phase,
                "the official runner PID evidence exceeded the reviewed match bound",
            ));
        }
        let pid = parse_canonical_u64(line)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                malformed_identity(phase, "the official runner PID evidence is not canonical")
            })?;
        if pids.contains(&pid) {
            return Err(malformed_identity(
                phase,
                "the official runner PID evidence contains a duplicate process",
            ));
        }
        pids.push(pid);
    }
    pids.sort_unstable();
    Ok(pids)
}

fn parse_single_line(
    output: &str,
    phase: ActionsRunnerReadinessPhase,
) -> Result<&str, ObservationProblem> {
    let Some(value) = output.strip_suffix('\n') else {
        return Err(malformed_identity(
            phase,
            "the official runner evidence is not one complete line",
        ));
    };
    if value.is_empty()
        || value
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(malformed_identity(
            phase,
            "the official runner evidence is not one canonical line",
        ));
    }
    Ok(value)
}

fn parse_filesystem_identity(
    output: &str,
    phase: ActionsRunnerReadinessPhase,
) -> Result<LimaFilesystemObjectIdentity, ObservationProblem> {
    let value = parse_single_line(output, phase)?;
    let Some((device, inode)) = value.split_once(':') else {
        return Err(malformed_identity(
            phase,
            "the official runner filesystem identity is malformed",
        ));
    };
    if inode.contains(':') {
        return Err(malformed_identity(
            phase,
            "the official runner filesystem identity is malformed",
        ));
    }
    let device_id = parse_canonical_u64(device).ok_or_else(|| {
        malformed_identity(phase, "the official runner device identity is malformed")
    })?;
    let inode = parse_canonical_u64(inode)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            malformed_identity(phase, "the official runner inode identity is malformed")
        })?;
    Ok(LimaFilesystemObjectIdentity { device_id, inode })
}

fn parse_private_sha256(output: &str) -> Result<Sha256Digest, ObservationProblem> {
    let value = parse_single_line(
        output,
        ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
    )?;
    let Some((digest, path)) = value.split_once("  ") else {
        return Err(malformed_identity(
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            "the official runner configuration digest is malformed",
        ));
    };
    if path != PROCESS_REDACTED_PATH || digest.len() != 64 || !digest.bytes().all(is_lower_hex) {
        return Err(malformed_identity(
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            "the official runner configuration digest is malformed",
        ));
    }
    Sha256Digest::parse(&format!("sha256:{digest}")).map_err(|_| {
        malformed_identity(
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            "the official runner configuration digest is malformed",
        )
    })
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.len() > 1 && value.starts_with('0')
    {
        return None;
    }
    value.parse().ok()
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn validate_private_path(
    path: PathBuf,
    allow_root: bool,
) -> Result<PathBuf, ActionsRunnerReadinessFailure> {
    if !valid_absolute_path(&path, allow_root) {
        return Err(input_failure(
            "reviewed runner paths must be bounded canonical absolute UTF-8 paths",
        ));
    }
    Ok(path)
}

fn valid_absolute_path(path: &Path, allow_root: bool) -> bool {
    let Some(value) = path.to_str() else {
        return false;
    };
    if !path.is_absolute()
        || value.len() > MAX_PATH_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
        || value.ends_with('/') && value != "/"
        || value.get(1..).is_some_and(|rest| rest.contains("//"))
        || value
            .split('/')
            .any(|component| matches!(component, "." | ".."))
        || !allow_root && value == "/"
    {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn strict_descendant(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

fn exact_path(path: &Path) -> &str {
    path.to_str()
        .expect("reviewed official runner paths are validated UTF-8")
}

fn input_failure(message: &'static str) -> ActionsRunnerReadinessFailure {
    ActionsRunnerReadinessFailure::from_problem(
        ObservationProblem::new(
            ActionsRunnerReadinessRefusalCode::InvalidInput,
            ActionsRunnerReadinessPhase::InputValidation,
            message,
        ),
        ActionsRunnerReadinessPrivateEvidence::default(),
    )
}

const fn command_failed(
    phase: ActionsRunnerReadinessPhase,
    message: &'static str,
) -> ObservationProblem {
    ObservationProblem::new(
        ActionsRunnerReadinessRefusalCode::CommandFailed,
        phase,
        message,
    )
}

const fn malformed_identity(
    phase: ActionsRunnerReadinessPhase,
    message: &'static str,
) -> ObservationProblem {
    ObservationProblem::new(
        ActionsRunnerReadinessRefusalCode::MalformedIdentityEvidence,
        phase,
        message,
    )
}

const fn clock_problem() -> ObservationProblem {
    ObservationProblem::new(
        ActionsRunnerReadinessRefusalCode::ClockFailure,
        ActionsRunnerReadinessPhase::Freshness,
        "the runner-readiness observation clock is unavailable or reversed",
    )
}

#[cfg(test)]
mod tests;
