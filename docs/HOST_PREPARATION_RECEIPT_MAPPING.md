# Host-preparation receipt mapping

This slice maps one terminal `HostPreparationExecutionReport` into the external execution receipt v1 contract.

The implementation lives in `src/host_preparation_receipt.rs`.

## Caller-supplied execution context

The durable execution report does not currently carry every external receipt identity. The caller must supply a validated `HostPreparationReceiptContext` containing:

- the durable `JournalId` created before the first checkpoint;
- the SHA-256 digest of the exact reviewed source identity;
- the canonical start time;
- the canonical terminal time.

The mapper does not generate substitute identities from phase names, action summaries, filesystem paths, or mutable host state. A later execution-boundary slice must prove that the supplied journal ID, digest, and timestamps came from the same durable execution before publishing the receipt.

## Public mapping

The mapper retains:

- canonical repository identity from the reviewed source report;
- phase identity;
- action identity;
- execution lane;
- rollback class;
- typed terminal action outcome;
- generic stable `action-execution-failed` or `action-rollback-failed` codes;
- fresh-observation barrier identities after a completed phase;
- deferred action identities;
- the report disposition.

Pending records after a failed action map to `not_run`. `executing` and `rollback_in_progress` records are non-terminal and fail closed.

The mapper never parses journal prose to recover failure authority. Current journal messages combine public codes with human-oriented summaries, so receipt v1 uses stable generic codes until a later typed journal field is reviewed.

## Privacy boundary

The mapper does not copy:

- executable or filesystem paths;
- action summaries;
- precondition evidence;
- journal messages;
- barrier or deferred-action summaries;
- complete host observations;
- commands, environment values, stdout, stderr, or credentials.

The reviewed source identity may contain private paths. Only its caller-supplied SHA-256 digest enters the external receipt.

All mapping failures use bounded fixed prose and do not include repository values, action IDs, journal messages, or source paths.

## Semantic checks

The mapper rejects:

- unsupported host-preparation execution schemas;
- unsupported journal schemas;
- non-canonical repository identities;
- non-terminal journal records;
- completed dispositions with failed, skipped, rolled-back, compensated, rollback-failed, or not-run actions;
- fresh-observation dispositions without a completed journal and explicit barrier identities;
- action-failed dispositions without a failed or rollback-failed action;
- invalid or duplicate receipt tokens;
- invalid caller-supplied timestamps or receipt identities through the receipt constructor.

## Deferred work

This slice is pure mapping only. It does not:

- change durable host execution;
- create journal IDs;
- compute the reviewed-source digest;
- add start or terminal clocks to execution;
- persist receipts;
- provide read-back by execution ID;
- change `host prepare` JSON output;
- send receipts to Stensibly, Proofwake, or another service.

The next local slice should create the journal ID and clock boundaries at execution start, compute the canonical private source digest, publish the receipt atomically beside the durable journal, and define exact execution-ID replay/conflict behaviour before adding CLI or network consumers.
