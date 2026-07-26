# SmolRunner implementation wave — 2026-07-26

This document coordinates the next five implementation lanes. It records ownership, scope, dependencies, merge order, handoff requirements, and copy-paste assignments for agents joining the wave.

## Goal

Turn the current MacBook/Lima work into a dependable agent workflow:

```text
prepare the host
→ prepare the repository workspace
→ invoke a named verification profile
→ produce an exact tested result
→ hand that result to a credentialed publisher
```

The immediate wave also recovers the two draft pull requests that already contain useful implementation work.

## Current repository state

- Issue #139 is complete through merged PR #140.
- Issue #144 is the active host-preparation execution thread.
- PR #146 contains the durable execution slice for one confirmed host-preparation phase. Its current blocker is formatting and branch reconciliation.
- PR #147 contains the Renderprove command/evidence adapter. Its current blocker is Clippy and branch reconciliation.
- Issues #117, #148, and #150 capture the workspace bootstrap, named verification profile, and exact-commit publication boundaries exposed by the recent Codex Lima validation.
- Issue #149 follows this wave. It should consume the contracts produced by #148 and #150 instead of inventing parallel profile and promotion models.

## Ownership

| Agent | Lane | Primary target | Status |
| --- | --- | --- | --- |
| Agent 1 — current ChatGPT thread | Recover durable host-preparation execution | PR #146 / issue #144 | Claimed |
| Agent 2 | Recover Renderprove command adapter | PR #147 / issue #125 | Available |
| Agent 3 | Named verification profile contract | Issue #148 | Available |
| Agent 4 | Exact-commit runner-to-publisher handoff | Issue #150 | Available |
| Agent 5 | Repository-owned bootstrap contract | Issue #117 | Available |

Agents should announce their lane on the linked issue or pull request before editing. Each lane has one primary owner. Cross-lane review stays focused on shared contracts and concrete conflicts.

## Dependency map

```text
PR #146 ───────────────→ host prepare CLI follow-on under #144

PR #147 ───────────────→ later Renderprove subprocess/artifact slices under #125

Issue #117 ── vocabulary and capability receipt ──┐
                                                   ├─→ Issue #149 canary upgrades
Issue #148 ── verification profile and receipt ───┤
                                                   │
Issue #150 ── tested-result publication receipt ──┘
```

PR #146 and PR #147 can proceed independently.

Issues #117 and #148 should share names for capabilities, workspace identity, profile readiness, deviations, and result receipts. Their authority remains separate:

- #117 belongs to the repository and prepares a workspace.
- #148 belongs to the runner/operator layer and invokes a reviewed repository verification contract.

Issue #150 may begin its pure planning and receipt types while #148 is in progress. Its eventual publish adapter should consume an immutable verification result rather than free-form terminal claims.

## Merge order

1. Recover and merge PR #146.
2. Recover and merge PR #147.
3. Land the first pure slices for #117, #148, and #150 in whichever order reaches complete verification first.
4. Reconcile vocabulary across those three lanes through narrow follow-up changes.
5. Begin #149 after profile, capability, and publication identities are stable enough to reuse.
6. Continue issue #144 toward the explicit elevated `host prepare` CLI after PR #146 is merged.

## Shared-file coordination

Likely conflict points:

- `src/lib.rs`
- manifest/reference documentation
- `AGENTS.md`
- `docs/ROADMAP.md`
- CI wrapper validation

Prefer new focused modules and documents. Keep `src/lib.rs` edits to module exports. Rebase immediately before final verification and inspect the full merged diff.

Temporary diagnostic workflows belong only on recovery branches and should disappear before merge unless the final PR explains a lasting repository-wide need.

## Common delivery requirements

Every implementation PR in this wave should:

- begin from current `main` or rebase onto it before final verification;
- keep one reviewable contract or adapter per PR;
- preserve human and JSON output from the same typed report;
- keep ambient credentials, private paths, raw repository contents, and broad environment dumps out of public output;
- use exact immutable identities wherever authority crosses a process, workspace, runner, or publisher boundary;
- fail closed on unknown, conflicting, stale, widened, or internally inconsistent evidence;
- document excluded authority explicitly;
- run the complete required checks from `AGENTS.md`;
- record the exact tested head SHA and complete changed-file list in the PR description;
- read the complete final diff before marking the PR ready;
- leave a handoff comment containing completed work, exact verification, unresolved risks, and the next smallest slice.

## Agent 1 assignment — claimed

**Target:** PR #146, progressing issue #144.

Recover the existing durable host-preparation phase execution branch. Preserve the branch’s current typed execution boundary and turn it into a clean reviewable PR.

Required work:

1. Rebase `codex/issue-144-durable-phase-execution` onto current `main`.
2. Apply repository formatting and remove the temporary `issue-144-split-format` workflow.
3. Run the complete locked verification suite and disposable Linux acceptance.
4. Review decision confirmation, durable-plan validation, checkpoint failure handling, action failure handling, fresh-observation barriers, deferred actions, and public redaction.
5. Confirm that this slice executes exactly one constructor-confirmed typed phase through injected runner/checkpoint traits.
6. Keep CLI wiring, state-store selection, executable discovery, filesystem opening, runner registration, generic apply behavior, automatic retry, and multi-phase continuation outside this PR.
7. Update the PR description with the exact final head, rebased base, changed-file list, verification commands, and remaining #144 work.
8. Mark ready after every required check passes.

## Agent 2 assignment — available

**Target:** PR #147, progressing issue #125.

Recover the Renderprove command/evidence adapter from its existing draft branch.

Required work:

1. Rebase `feat/renderprove-command-adapter` onto current `main`.
2. Restore the canonical shared CI workflow and remove branch-only diagnostic edits.
3. Reproduce the current Clippy failure and fix its cause without widening the adapter.
4. Preserve the pure boundary: reviewed `CommandSpec` planning and exact `ExecutionRecord` binding only.
5. Verify runner-user entry through reviewed `runuser`, an empty inner environment, explicit identity variables, fixed wrapper/suite selection, disjoint trusted checkout and disposable workspace, loopback-only review, and private diagnostic retention.
6. Verify that public JSON and `Debug` output exclude workspace, checkout, evidence, home, runtime, wrapper, stdout, and stderr values.
7. Keep subprocess execution, executable proof, filesystem observation, container lifecycle, browser authority, cancellation execution, cleanup, evidence reading, artifact export, and deployed-origin networking outside this PR.
8. Run the complete required suite, record the exact final head and changed-file list, then mark the PR ready.

## Agent 3 assignment — available

**Target:** issue #148.

Implement the first pure contract for named reusable verification profiles on warm runners.

Required work:

1. Comment on #148 with the exact first-slice boundary before coding.
2. Add versioned typed models for profile identity, runner-owned workspace identity, immutable source inputs, capability requirements, repository command identity, test/build scope, resource defaults, cache identity, timeout, mutation policy, and publication policy.
3. Distinguish required capabilities, optional capabilities, and repository-approved equivalent commands.
4. Model exact test scope so a library test, one integration-test binary, and a whole package remain distinct.
5. Represent read-only, workspace-reset, local-commit, and publication authority explicitly.
6. Add pure preflight/result types covering ready, ready-with-declared-deviations, and blocked outcomes.
7. Include bounded phase/result fields for resolved refs, tested tree or commit, exact command identity, timings, cache use, skips, retries, deviations, cleanup, and publication state.
8. Add deterministic tests for path escape, moving refs, missing required tools, undeclared fallback, widened build scope, insufficient memory policy, invalid concurrency, dirty workspace policy, and secret/private-path redaction.
9. Keep shell execution, tool installation, workspace mutation, Git publication, GitHub Actions triggering, and generic workflow syntax outside the first PR.
10. Update #148 with the exact tested head and the next adapter slice after merge.

## Agent 4 assignment — available

**Target:** issue #150.

Turn the successful exact Git bundle experiment into a typed runner-to-publisher handoff contract.

Required work:

1. Comment on #150 with the exact first-slice boundary before coding.
2. Add separate versioned models for planning, export, transfer, import, publish authorization, publication result, and cleanup.
3. Bind a candidate commit to expected parent/ancestry, tree, changed-path allowlist, clean-worktree evidence, repository/ref identity, and immutable verification receipt identity.
4. Preserve commit SHA and tree SHA across every phase.
5. Model fast-forward-only publication against an expected remote parent.
6. Produce bounded human and JSON reports with runner identity, workspace identity, publisher identity class, target ref, candidate/final SHA, changed paths, verification result, and cleanup result.
7. Add automated refusal tests for moved remote ref, dirty workspace, extra changed path, ambiguous candidate, altered transfer package, missing verification receipt, mismatched tree, failed import, and incomplete cleanup.
8. Keep publisher credentials outside runner-facing models and public output.
9. Keep real transport, Git process execution, and remote push outside the first pure PR unless a smaller reviewed adapter already exists and can be reused without broad shell authority.
10. Use the recorded 2026-07-26 experiment as the positive example and document the next executable adapter slice.

## Agent 5 assignment — available

**Target:** issue #117.

Define and prove the repository-owned one-command bootstrap contract, beginning with SmolRunner itself.

Required work:

1. Comment on #117 with the proposed canonical entrypoint and first-slice boundary before coding.
2. Add a short design document defining command name, working directory, environment policy, idempotence, credential boundary, outputs, failure classes, and capability receipt.
3. Add the canonical bootstrap/preflight entrypoint to SmolRunner.
4. Emit a machine-readable receipt containing repository root identity, required and optional tool/version observations, repository-native verification backend availability, formatter capabilities by domain, declared cache paths and ownership expectations, memory/swap observations or requirements, recommended concurrency, Git identity/publication readiness only when requested, and exact next profile names.
5. Return ready, ready-with-declared-deviations, or blocked.
6. Keep host package installation, `sudo`, global credential changes, shell-profile edits, undeclared global tool installation, and production deployment outside the command.
7. Prove a second run is safe, lockfiles remain unchanged, and the checkout remains clean.
8. Validate the command from a fresh Ubuntu/Lima checkout and in CI where practical.
9. Coordinate field names with the #148 owner while keeping repository bootstrap authority separate from runner profile authority.
10. Update #117 with the exact tested head, observed cold/repeated behavior, and the next integration slice.

## Handoff template

Use this format when pausing or completing a lane:

```text
Lane:
Issue/PR:
Branch:
Exact head SHA:
Base SHA:
Completed:
Changed files:
Verification commands and results:
Known risks or unresolved failures:
Excluded authority preserved:
Recommended next smallest slice:
```

## Later queue

After this wave:

- complete the explicit elevated `host prepare` CLI under #144;
- implement official GitHub Actions runner installation, registration, service lifecycle, drain, update, disable, and removal;
- begin #149 canary upgrades using the profile and publication contracts from this wave;
- defer broad agent control APIs, fleet repair, previews, and provider selection until the runner lifecycle and disposable verification path are dependable.
