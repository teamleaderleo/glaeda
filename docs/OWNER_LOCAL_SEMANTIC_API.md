# Owner-local semantic API v1

Issue #1012 defines the Glaeda-owned semantic seam used by direct owner/chat/CLI callers. Transport
authentication stays outside this contract. Physical execution stays inside the existing Glaeda
admission and owned-Linux task path.

```text
caller authentication / transport
            |
            v
glaeda-semantic-request/v1
            |
            v
reviewed Glaeda operation adapter
            |
            v
Glaeda source / admission / capability checks
            |
            v
existing owned execution primitive
            |
            v
glaeda-semantic-receipt/v1
```

The implementation is `scripts/provider_neutral_request.py`. It is deliberately pure: decoding,
semantic compilation, replay identity and receipt projection only. It performs no Git, filesystem,
network, process, scheduler, credential or host mutation.

## First operations

The v1 vocabulary is closed:

| Semantic operation | Existing Glaeda owner | Effect class |
| --- | --- | --- |
| `capabilities` | semantic contract | read-only |
| `status` | `owned_linux_admission.observe()` | read-only |
| `repo_query` | `repo-query/v1` | read-only bounded Git observation |
| `verify_named` | `verify-focused/v1` + owned admission + `owned_linux_task` | bounded source execution |

`command_argv` is absent. Glaeda does not yet own a reviewed general command profile that satisfies
the requested executable/cwd/resource/network/environment/source/replay boundary. The Git transport
may parse an argv-shaped future request, but semantic v1 cannot compile it into execution authority.

The provider-neutral request is canonical JSON:

```json
{
  "document_type": "glaeda-semantic-request",
  "schema_version": 1,
  "request_id": "owner-1012-request-0001",
  "operation": "verify_named",
  "source": {
    "repository": "teamleaderleo/glaeda",
    "commit": "<exact 40-hex commit>",
    "tree": "<exact 40-hex tree>"
  },
  "parameters": {
    "profile_id": "verify-focused/v1"
  }
}
```

`capabilities` and `status` carry `source: null` and empty parameters. `repo_query` carries the
same exact source plus an exact base commit and a patch inclusion ceiling capped at 8 KiB.

## Identity and replay

`request_id` is the explicit Glaeda request identity. It is separate from a Git branch/ref, issue,
chat turn, Stensibly work item, CMUX work ref, MCP call id or CLI process.

The canonical request bytes also produce `request_sha256`.

Rules:

- same `request_id` + same canonical semantics may replay one existing receipt;
- same `request_id` + different semantics is `request_conflict`;
- different request ids remain different physical work even when source/profile bytes happen to
  match;
- two transports share physical work only when both deliberately carry the same accepted Glaeda
  `request_id` and the same request digest.

For `verify_named`, the semantic request identity is included in the verifier command fingerprint.
Transport-only correlation fields are excluded.

This corrects an easy failure mode in caller-specific adapters: equality of source/profile bytes is
insufficient evidence that two callers meant one physical attempt.

## Operation mapping

### capabilities

Returns `glaeda-semantic-capabilities/v1`: the closed operation vocabulary, contract generation,
reviewed profile names, and the explicit fact that `command_argv` is unavailable.

This is semantic capability discovery. Current-node availability belongs to `status`.

### status

A host adapter asks the existing owned-Linux admission observer and wraps only its bounded
`glaeda-owned-admission-observation/v1` result. The remote semantic request contains no admission
root, host path, cgroup value, process identity or policy override.

Held, draining, pressured, reserved or unavailable states remain Glaeda observations. The status
receipt grants no execution or redispatch authority.

### repo_query

The semantic request names exact repository/base/head/tree identities and a bounded patch ceiling.
A host adapter invokes the existing fixed `glaeda-repo-query` / `repo-query/v1` implementation
against its operator-owned resident checkout mapping.

The semantic layer accepts only a matching typed report:

- `document_type=glaeda-resident-repo-query`;
- `profile_id=repo-query/v1`;
- `authority=observation_only`;
- exact requested repository/base/head/tree;
- bounded result bytes.

No Git argv, ref, fetch, remote URL or checkout path enters the semantic request.

### verify_named

v1 accepts only `verify-focused/v1`. Glaeda resolves the exact profile generation, fixed resource
class, network-none/minimal-environment policy, deadline and output ceiling.

The existing verifier remains the executor. A physical adapter still supplies its own reviewed,
operator-installed repository/state/tool/admission bindings, revalidates exact source locally, and
enters the same owned-Linux admission/task boundary used by other callers.

The semantic receipt projects only bounded content-free terminal evidence from the existing typed
verification receipt: terminal class, physical start/settle times, output digest/byte count, process
settlement, cleanup and the exact workload receipt digest.

## Receipt contract

Every operation returns `glaeda-semantic-receipt/v1` with:

```text
request_id
request_sha256
operation
exact source or null
state
resolved operation/profile
bounded typed result or digest
refusal code where applicable
authority = all false
```

States are closed to:

```text
planned
waiting
refused
ambiguous
succeeded
failed
timed_out
cleanup_incomplete
```

`receipt.inspect` is the thin-connector name for validating this closed receipt. The implementation
is `inspect_receipt()`; inspection grants no replay, cleanup or execution authority.

## Transport adapters

A transport compiles into the same semantic request and returns the same semantic receipt.

Future direct connector/MCP names map as:

```text
local.capabilities -> capabilities
local.status       -> status
local.query        -> repo_query
local.verify       -> verify_named
receipt.inspect    -> inspect semantic receipt
```

The connector owns authentication and transport only. It contains no SSH client, remote shell,
raw filesystem API, resource scheduler or cache authority.

The GitHub rendezvous from #967/#1012 can map its immutable direct request id to the semantic
`request_id`, validate its transport-specific expiry/provenance, then compile the action into this
contract before physical admission. GitHub Actions, CLI and Stensibly can compile the same way.
A transport-specific request/result journal stays transport evidence; physical attempt truth remains
with Glaeda.

## Availability behavior

| Condition | Semantic behavior |
| --- | --- |
| Stensibly unavailable | direct Glaeda semantics are unchanged |
| Convex unavailable | direct Glaeda semantics are unchanged |
| GitHub unavailable | Git rendezvous pauses; CLI/direct connector can still operate |
| connector unavailable | Git rendezvous can still operate |
| node held | status reports wait/refusal; fresh verification does not launch |
| node draining | status reports wait; fresh verification does not launch |
| node pressured/capacity-limited | status reports wait; fresh verification does not launch |
| requested resident source unavailable/cold | physical adapter refuses/waits according to its reviewed source contract |
| duplicate exact request | replay the matching receipt |
| request id reused with drifted semantics | refuse `request_conflict` |
| surviving intent without terminal truth | return `ambiguous`; never infer permission to redispatch |

Transport loss never changes the physical admission state. A transport retry cannot create a second
uncertain attempt.

## Physical dogfood

The direct Git transport has already demonstrated named focused verification through the existing
Glaeda verifier without a GitHub Actions allocation. The next same-semantics comparison should carry
one explicit semantic `request_id` through:

1. direct Git/owner-local dispatch;
2. GitHub Actions using the same named verification operation and exact source;
3. receipt comparison for source/profile/physical admission family;
4. confirmation that the direct request has no Actions run/job identity.

Record request-to-admitted, admitted-to-first-command, useful-result, result-return/publication,
transport/API overhead, execution overhead and cleanup separately.

This lane remains independent from adaptive placement and automatic workload selection.
