# Host-preparation execution receipt binding

This layer binds receipt authority to one reviewed execution before the first durable mutation checkpoint.

## Two-stage boundary

`HostPreparationReceiptBinding::begin()` accepts:

- the durable execution or journal ID that the checkpoint writer will use;
- the exact retained `HostReadinessSourceIdentity` reviewed by the confirmed proposal;
- the exact executable phase ID;
- an explicit canonical start timestamp captured before the first checkpoint.

It validates the phase identity and computes one domain-separated SHA-256 digest over a versioned deterministic source document. The private source is streamed through a bounded digest writer and is never retained as a serialized byte buffer.

`finish()` consumes the binding after terminal execution and accepts:

- the terminal `HostPreparationExecutionReport`;
- an explicit terminal timestamp captured at the terminal report boundary.

It rejects the report unless:

- the terminal timestamp is not earlier than the bound start;
- the terminal phase exactly matches the pre-execution phase;
- re-digesting the report's retained source identity exactly matches the pre-execution source digest;
- the terminal report satisfies execution receipt v1 through the existing mapper.

Consuming the binding prevents one pre-execution context from authorizing multiple receipts.

## Source digest document

The digest input is deterministic JSON for this fixed document:

```json
{
  "document_type": "smolrunner_host_preparation_source",
  "schema_version": 1,
  "source": {}
}
```

The `source` value is the exact typed `HostReadinessSourceIdentity`, including private reviewed executable paths and bounded host-readiness state. JSON bytes are streamed directly into SHA-256 and are never returned or persisted by this layer.

The streaming writer stops once the digest input would exceed 65,536 bytes. Oversized input returns one fixed `source_too_large` error without disclosing source content. The public result uses canonical `sha256:<64 lowercase hex>` form.

The v1 compatibility fixture pins the complete digest for one representative source document. Changing schema identity, field ordering, serialization, or any retained source field—including a private executable path—changes the digest and causes either a compatibility-test failure or terminal source mismatch.

## Privacy boundary

The binding retains only:

- execution ID;
- SHA-256 source digest;
- phase ID;
- start timestamp.

Its public errors and `Debug` output exclude:

- executable and filesystem paths;
- complete source observations;
- action summaries and precondition evidence;
- journal messages;
- command values, environment values, process output, and credentials.

## Dependency boundary

SHA-256 uses `sha2 0.10`, backed by the standard RustCrypto `digest` stack. This adds no network transport, external process, dynamic provider, credential access, or operating-system command dependency.

## Boundary of this slice

This layer does not yet:

- change `host prepare`;
- choose or capture the system clock;
- generate the execution ID;
- install the binding into the journal checkpoint path;
- publish the finished receipt;
- change CLI JSON output;
- add receipt read-back commands;
- send evidence to Stensibly, Proofwake, or another service.

The next live-execution slice should create the binding after installation resolution and phase classification but before the first journal checkpoint, use the same execution ID for `StateStoreJournalCheckpoint`, capture the terminal time immediately after a terminal report, finish the receipt, and publish through the durable receipt store.
