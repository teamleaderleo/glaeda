# Execution admission and reservation visibility

`src/execution_admission.rs` defines the pure schema-versioned admission contract for issue #157. It reuses the merged `VerificationProfileId` authority and binds one immutable request and runner-profile identity to requested resources, observed host capacity, applied limits, queue visibility, reservation identity/generation/start/expiry, cancellation or drain acknowledgement, and fallback eligibility.

Every non-terminal state accepts monotonic same-state observations, so a runtime can publish queue changes and admitted, reserved, starting, running, or draining heartbeats without changing authority. `draining` is active rather than terminal; cancellation and drain completion must pass through it before the matching terminal `unavailable` result. Only `unavailable` is terminal.

Reservation lifetime is measured from the immutable `reserved_at` boundary, not from later host-capacity observations. This permits fresh capacity evidence while preserving exact reservation identity, generation, start, expiry, and applied limits.

The module performs no scheduling, polling, persistence, host probing, resource mutation, process execution, cancellation delivery, fallback execution, Stensibly operation, or GitHub runner control. A later adapter must collect and persist the evidence before passing typed observations into this contract.

Public serialization contains only bounded identifiers, enums, timestamps, and numeric resource facts. It contains no paths, credentials, environment values, command output, or arbitrary logs, and validation errors never echo rejected input.

Canonical Verify and Linux acceptance runs on the exact pull-request head remain the acceptance boundary for this pure contract.
