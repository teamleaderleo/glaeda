# Renderprove artifact receipt binding

`src/renderprove_artifact_binding.rs` is the pure boundary between one exact typed Renderprove execution receipt and the final verification/artifact report.

The binder requires all evidence that must be known before artifact identities can be finalized:

- the exact `RenderproveExecutionReceipt`, which retains the reviewed command, request, process outcome, and private diagnostics;
- an explicit cleanup outcome;
- a typed sanitized-receipt assessment;
- bounded content-addressed evidence artifact identities.

A present sanitized receipt repeats the exact source, project OCI image, Renderprove worker image, and manifest identities from the retained execution request. Binding fails if any identity drifts. Exactly one `sanitized_receipt` artifact must match the typed receipt digest. Missing or invalid receipt assessments cannot include a sanitized-receipt artifact.

The existing `finalize_renderprove_verification` contract remains authoritative for process, cleanup, receipt, artifact-count, evidence-budget, visibility, failure, and disposition rules. This adapter does not duplicate those policies; it supplies them with the process outcome from the exact execution receipt and converts the typed sanitized receipt into the existing receipt outcome.

The serialized final receipt contains the exact public request identities, process and cleanup outcomes, sanitized receipt assessment, disposition, failures, and public artifact identities. The exact execution receipt and private artifact identities remain retained but are skipped during serialization and redacted from `Debug` output. A successful process therefore cannot pass unless the exact bound receipt is passing and its sanitized artifact digest matches.

This module performs no subprocess execution, filesystem access, hashing, receipt parsing, cleanup, artifact export, browser or container control, networking, credential access, deployment, or publication. It is independent of the blocked path-based subprocess adapter in PR #183.
