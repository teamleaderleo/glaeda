# CMUX repository-owned workload profiles

CMUX owns the meaning of CMUX workload profiles. Glaeda may admit, place, execute, cache, time, and recover those workloads without copying their command lines or redefining pass/fail semantics.

The paired CMUX implementation is tracked by `manaflow-ai/cmux#13411`.

## Request boundary

`scripts/cmux_workload_request.py` accepts a bounded semantic request containing:

- exact `manaflow-ai/cmux` commit and tree;
- CMUX profile ID and explicit semantic generation;
- benchmark state class;
- bounded integer semantic parameters, such as an app-host shard number;
- caller correlation and an advisory reuse hint.

It rejects caller-controlled argv, shell text, cwd, host paths, backend/machine choice, resource values, cache roots, attempt identity, or cleanup authority.

A planned request resolves only this fixed adapter:

```json
{
  "kind": "cmux-repository-profile/v1",
  "repository_runner": "scripts/ci/cmux_workload_profile.py",
  "semantic_result_contract": "cmux-workload-result/v1"
}
```

Glaeda does not keep a copy of the CMUX registry. Profile validity is checked after exact source materialization by the runner inside that CMUX tree.

## Physical execution

A physical adapter follows this sequence:

1. materialize the exact requested CMUX commit/tree;
2. choose an eligible backend and admitted cache/state class from Glaeda-owned observations;
3. run the checked-in CMUX profile runner's `plan` command and require the same source, profile ID/generation, state class, and semantic parameters;
4. allocate a task-private state root and any reviewed runtime inputs;
5. run the same CMUX profile runner and retain the exact canonical `cmux-workload-result/v1`;
6. wrap its digest and terminal state in Glaeda's physical receipt.

The generic repository adapter may translate the semantic request into the fixed CMUX runner interface. It does not reconstruct Xcode, test-selector, package, release, or developer-build commands.

Runtime-input paths stay physical. Their content identities are published by the CMUX semantic result when the profile contract requires them.

## Identity

Three identities remain separate:

- caller correlation: external request/work references;
- semantic execution binding: exact source + CMUX profile/generation + CMUX-owned environment class + state class + semantic parameters + fixed adapter generation;
- physical attempt: Glaeda backend, node, leases, cache placement, resources, timing, cleanup, and recovery state.

Caller correlation and reuse hints do not mint another execution binding. A profile generation change does.

## Result correlation

`observe` accepts only canonical `cmux-workload-result/v1` bytes and verifies:

- exact repository/commit/tree;
- exact profile ID/generation;
- bounded CMUX-owned environment class, bound into the semantic comparison identity;
- exact semantic parameters;
- requested benchmark state class;
- presence of the CMUX semantic validator, artifact collection, and cleanup evidence.

It validates the shared `cmux-workload-result/v1` integrity envelope before projection: the result schema is closed, nested identity/evidence shapes are typed and bounded by the document ceiling, the toolchain identity must match its observations, timestamps and cleanup are self-consistent, and a `passed` result must have exit zero, complete required-artifact validation, and clean process settlement. These are interface-integrity checks; Glaeda does not re-run or reinterpret CMUX's profile-specific validators.

It then projects the CMUX terminal vocabulary without redefining profile semantics:

- `passed` -> `succeeded`;
- `failed` -> `failed`;
- `timed_out` -> `timed_out`;
- `ambiguous` -> `ambiguous`.

The outer observation stores the exact CMUX result digest. Fleet acceptance uses `glaeda-cmux-fleet-acceptance/v2` through `cmux_fleet.py accept-local`: Glaeda owns the local CMUX runner process, captures its result in a private attempt directory, then performs a fresh read-only bootstrap observation on the same node. The durable receipt binds that fresh-bootstrap digest, CMUX environment class/toolchain identity, and an opaque Glaeda-local attempt digest. Externally supplied semantic results can be validated, but they cannot mint an accepted fleet receipt.

## Fleet roles and routing

The first CMUX mapping is:

```yaml
cmux_linux_ci:
  acceptance:
    profile: cmux.ci.guard
    generation: 1

cmux_macos_native_build:
  acceptance:
    profile: cmux.macos.dev-check
    generation: 1
```

Role acceptance consumes the current profile identity from CMUX rather than a Glaeda-owned recipe. Routing experiments can compare backends only when the CMUX semantic comparison identity and benchmark context agree.

This gives #148 a repository-owned named-profile source, #546 comparable routing evidence, #547 a stable semantic operation for optimization, and #1056 an exact acceptance workload. #1057's caller/orchestrator identity remains independent from this workload identity.

## Synthetic proof

`docs/experiments/cmux-workload-profile/` contains a request and its deterministic planned result. It proves the GitHub/repository-only request boundary without claiming physical execution.

Run:

```sh
python3 scripts/cmux_workload_request.py plan \
  < docs/experiments/cmux-workload-profile/request.json \
  > /tmp/cmux-workload-plan.json

cmp /tmp/cmux-workload-plan.json \
  docs/experiments/cmux-workload-profile/plan.json

python3 scripts/test-cmux-workload-request.py
```

Physical Mac acceptance remains a separate approved fleet experiment.
