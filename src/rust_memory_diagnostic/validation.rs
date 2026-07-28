fn validate_binding(
    field: &'static str,
    identity: &RustMemoryDiagnosticIdentity,
    binding: &RustObservationBinding,
) -> Result<(), RustMemoryDiagnosticError> {
    if binding.attempt_id != identity.attempt_id || binding.process_group != identity.process_group {
        return Err(RustMemoryDiagnosticError::new(
            field,
            "observation_identity_drift",
            "all observations must bind the exact attempt and process-group generation",
        ));
    }
    Ok(())
}

fn validate_not_future(
    field: &'static str,
    observed_at: EpochMillis,
    classified_at: EpochMillis,
) -> Result<(), RustMemoryDiagnosticError> {
    if observed_at > classified_at {
        return Err(future_error(field));
    }
    Ok(())
}

fn validate_memory_bytes(
    field: &'static str,
    value: u64,
) -> Result<(), RustMemoryDiagnosticError> {
    if value > MAX_RUST_DIAGNOSTIC_MEMORY_BYTES {
        return Err(RustMemoryDiagnosticError::new(
            field,
            "memory_observation_exceeded",
            "memory observations must remain within the fixed public bound",
        ));
    }
    Ok(())
}

fn validate_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), RustMemoryDiagnosticError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.is_ascii()
        && !value.starts_with('-')
        && !value.ends_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(RustMemoryDiagnosticError::new(
            field,
            "invalid_identifier",
            "must be bounded safe ASCII letters, digits, '.', '_', or '-'",
        ));
    }
    Ok(())
}

fn future_error(field: &'static str) -> RustMemoryDiagnosticError {
    RustMemoryDiagnosticError::new(
        field,
        "future_observation",
        "observation time may not be later than its reviewed comparison boundary",
    )
}

fn stale_error(field: &'static str) -> RustMemoryDiagnosticError {
    RustMemoryDiagnosticError::new(
        field,
        "stale_observation",
        "observation exceeded the reviewed freshness bound",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl RustMemoryDiagnosticError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RustMemoryDiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.problem)
    }
}

impl std::error::Error for RustMemoryDiagnosticError {}

