from pathlib import Path
import re


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label} anchor count: {count}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


lib = Path("src/lib.rs")
replace_once(
    lib,
    "/// Pure repository-declared Rust build-scope and bounded resource-envelope contracts.\npub mod rust_verification_envelope;",
    "/// Pure classification of trusted bounded Rust memory-pressure observations.\npub mod rust_memory_diagnostic;\n/// Pure repository-declared Rust build-scope and bounded resource-envelope contracts.\npub mod rust_verification_envelope;",
    "lib export",
)

envelope = Path("src/rust_verification_envelope.rs")
replace_once(
    envelope,
    '''    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn scope(&self) -> &RustVerificationScope {''',
    '''    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn source(&self) -> &RustVerificationSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn command(&self) -> &RepositoryCommandIdentity {
        &self.command
    }

    #[must_use]
    pub const fn scope(&self) -> &RustVerificationScope {''',
    "envelope identity accessors",
)

model = Path("src/rust_memory_diagnostic/model.rs")
text = model.read_text(encoding="utf-8")
identity_pattern = re.compile(
    r"#\[derive\(Debug, Clone, PartialEq, Eq, Serialize\)\]\n"
    r"pub struct RustMemoryDiagnosticIdentity \{[\s\S]*?\n\}\n\n"
    r"impl RustMemoryDiagnosticIdentity \{[\s\S]*?\n\}\n\n"
    r"#\[derive\(Debug, Clone, PartialEq, Eq, Serialize\)\]\n"
    r"pub struct RustObservationBinding",
)
identity_replacement = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticIdentity {
    verification_profile_id: VerificationProfileId,
    source: RustVerificationSourceIdentity,
    command: RepositoryCommandIdentity,
    envelope_digest: Sha256Digest,
    attempt_id: RustVerificationAttemptId,
    process_group: RustProcessGroupIdentity,
}

impl RustMemoryDiagnosticIdentity {
    fn from_envelope(
        envelope: &RustVerificationEnvelope,
        envelope_digest: Sha256Digest,
        attempt_id: RustVerificationAttemptId,
        process_group: RustProcessGroupIdentity,
    ) -> Self {
        Self {
            verification_profile_id: envelope.profile_id().clone(),
            source: envelope.source().clone(),
            command: envelope.command().clone(),
            envelope_digest,
            attempt_id,
            process_group,
        }
    }

    #[must_use]
    pub const fn verification_profile_id(&self) -> &VerificationProfileId {
        &self.verification_profile_id
    }

    #[must_use]
    pub const fn source(&self) -> &RustVerificationSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn command(&self) -> &RepositoryCommandIdentity {
        &self.command
    }

    #[must_use]
    pub const fn envelope_digest(&self) -> &Sha256Digest {
        &self.envelope_digest
    }

    #[must_use]
    pub const fn attempt_id(&self) -> &RustVerificationAttemptId {
        &self.attempt_id
    }

    #[must_use]
    pub const fn process_group(&self) -> &RustProcessGroupIdentity {
        &self.process_group
    }

    #[must_use]
    pub fn observation_binding(&self) -> RustObservationBinding {
        RustObservationBinding {
            attempt_id: self.attempt_id.clone(),
            process_group: self.process_group.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustObservationBinding'''
text, count = identity_pattern.subn(identity_replacement, text, count=1)
if count != 1:
    raise SystemExit(f"identity replacement count: {count}")
text = text.replace(
    "    pub fn from_envelope(envelope: &RustVerificationEnvelope) -> Self {",
    "    fn from_envelope(envelope: &RustVerificationEnvelope) -> Self {",
    1,
)

authority_anchor = '''}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustDiagnosticTiming'''
binding_definition = '''}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticEnvelopeBinding {
    identity: RustMemoryDiagnosticIdentity,
    authority: RustMemoryAuthoritySnapshot,
}

impl RustMemoryDiagnosticEnvelopeBinding {
    #[must_use]
    pub fn from_envelope(
        envelope: &RustVerificationEnvelope,
        envelope_digest: Sha256Digest,
        attempt_id: RustVerificationAttemptId,
        process_group: RustProcessGroupIdentity,
    ) -> Self {
        Self {
            identity: RustMemoryDiagnosticIdentity::from_envelope(
                envelope,
                envelope_digest,
                attempt_id,
                process_group,
            ),
            authority: RustMemoryAuthoritySnapshot::from_envelope(envelope),
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &RustMemoryDiagnosticIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn authority(&self) -> &RustMemoryAuthoritySnapshot {
        &self.authority
    }

    #[must_use]
    pub fn observation_binding(&self) -> RustObservationBinding {
        self.identity.observation_binding()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustDiagnosticTiming'''
if text.count(authority_anchor) != 1:
    raise SystemExit(f"binding insertion count: {text.count(authority_anchor)}")
text = text.replace(authority_anchor, binding_definition, 1)

input_pattern = re.compile(
    r"#\[derive\(Debug, Clone, PartialEq, Eq, Serialize\)\]\n"
    r"pub struct RustMemoryDiagnosticInput \{[\s\S]*?\n\}\n\n"
    r"#\[derive\(Debug, Clone, Copy, PartialEq, Eq, Serialize\)\]",
)
input_replacement = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticInput {
    binding: RustMemoryDiagnosticEnvelopeBinding,
    timing: RustDiagnosticTiming,
    preflight: RustPreflightMemoryObservation,
    terminal: RustTerminalObservation,
    cgroup: RustCgroupMemoryEvidence,
}

impl RustMemoryDiagnosticInput {
    #[must_use]
    pub const fn new(
        binding: RustMemoryDiagnosticEnvelopeBinding,
        timing: RustDiagnosticTiming,
        preflight: RustPreflightMemoryObservation,
        terminal: RustTerminalObservation,
        cgroup: RustCgroupMemoryEvidence,
    ) -> Self {
        Self {
            binding,
            timing,
            preflight,
            terminal,
            cgroup,
        }
    }

    #[must_use]
    pub const fn envelope_binding(&self) -> &RustMemoryDiagnosticEnvelopeBinding {
        &self.binding
    }

    #[must_use]
    pub fn observation_binding(&self) -> RustObservationBinding {
        self.binding.observation_binding()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]'''
text, count = input_pattern.subn(input_replacement, text, count=1)
if count != 1:
    raise SystemExit(f"input replacement count: {count}")
model.write_text(text, encoding="utf-8")

classification = Path("src/rust_memory_diagnostic/classification.rs")
text = classification.read_text(encoding="utf-8")
text = text.replace("input.identity", "input.binding.identity")
text = text.replace("input.authority", "input.binding.authority")
classification.write_text(text, encoding="utf-8")

tests = Path("src/rust_memory_diagnostic/tests.rs")
text = tests.read_text(encoding="utf-8")
identity_pattern = re.compile(
    r"fn identity\(envelope: &RustVerificationEnvelope\) -> RustMemoryDiagnosticIdentity \{[\s\S]*?\n\}\n\nfn counters",
)
identity_replacement = '''fn envelope_binding(envelope: &RustVerificationEnvelope) -> RustMemoryDiagnosticEnvelopeBinding {
    RustMemoryDiagnosticEnvelopeBinding::from_envelope(
        envelope,
        digest('f'),
        RustVerificationAttemptId::parse("attempt-1").expect("attempt"),
        process_group(1),
    )
}

fn counters'''
text, count = identity_pattern.subn(identity_replacement, text, count=1)
if count != 1:
    raise SystemExit(f"test binding helper replacement count: {count}")

executed_pattern = re.compile(
    r"fn executed_input\([\s\S]*?\n\}\n\nfn refused_input",
)
executed_replacement = '''fn executed_input(
    envelope: &RustVerificationEnvelope,
    phase: RustExecutionPhase,
    termination: RustProcessTermination,
    after_events: RustMemoryEventCounters,
) -> RustMemoryDiagnosticInput {
    let envelope_binding = envelope_binding(envelope);
    let observation_binding = envelope_binding.observation_binding();
    RustMemoryDiagnosticInput::new(
        envelope_binding,
        RustDiagnosticTiming::new(epoch(2_200), Some(epoch(1_200)), 500, 500)
            .expect("timing"),
        RustPreflightMemoryObservation::new(
            observation_binding.clone(),
            epoch(1_000),
            5 * GIB,
            GIB,
        )
        .expect("preflight"),
        RustTerminalObservation::new(
            observation_binding.clone(),
            epoch(2_000),
            phase,
            termination,
        ),
        RustCgroupMemoryEvidence::Complete {
            before: cgroup_observation(
                observation_binding.clone(),
                1_100,
                512 * MIB,
                512 * MIB,
                counters(0, 0),
            ),
            after: cgroup_observation(
                observation_binding,
                2_100,
                256 * MIB,
                5 * GIB,
                after_events,
            ),
        },
    )
}

fn refused_input'''
text, count = executed_pattern.subn(executed_replacement, text, count=1)
if count != 1:
    raise SystemExit(f"executed fixture replacement count: {count}")

refused_pattern = re.compile(
    r"fn refused_input\(envelope: &RustVerificationEnvelope\) -> RustMemoryDiagnosticInput \{[\s\S]*?\n\}\n\n#\[test\]",
)
refused_replacement = '''fn refused_input(envelope: &RustVerificationEnvelope) -> RustMemoryDiagnosticInput {
    let envelope_binding = envelope_binding(envelope);
    let observation_binding = envelope_binding.observation_binding();
    RustMemoryDiagnosticInput::new(
        envelope_binding,
        RustDiagnosticTiming::new(epoch(1_200), None, 500, 500).expect("timing"),
        RustPreflightMemoryObservation::new(
            observation_binding.clone(),
            epoch(1_000),
            4 * GIB,
            GIB,
        )
        .expect("preflight"),
        RustTerminalObservation::new(
            observation_binding,
            epoch(1_200),
            RustExecutionPhase::NotStarted,
            RustProcessTermination::NotStarted,
        ),
        RustCgroupMemoryEvidence::NotCreated,
    )
}

#[test]
fn envelope_binding_derives_identity_and_authority_atomically() {
    let envelope = envelope(12_345);
    let binding = envelope_binding(&envelope);

    assert_eq!(binding.identity().verification_profile_id(), envelope.profile_id());
    assert_eq!(binding.identity().source(), envelope.source());
    assert_eq!(binding.identity().command(), envelope.command());
    assert_eq!(binding.identity().envelope_digest(), &digest('f'));
    assert_eq!(binding.authority().maximum_execution_millis(), 12_345);
    assert_eq!(binding.authority().reserved_memory_bytes(), 6 * GIB);
}

#[test]'''
text, count = refused_pattern.subn(refused_replacement, text, count=1)
if count != 1:
    raise SystemExit(f"refused fixture replacement count: {count}")
text = text.replace("input.identity.observation_binding()", "input.observation_binding()")
tests.write_text(text, encoding="utf-8")
