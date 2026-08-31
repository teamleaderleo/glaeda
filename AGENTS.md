# Glaeda agent entry

Use **Glaeda** / `glaeda` for current surfaces. Keep **SmolRunner** only in
truthful historical evidence, exact v1 identities, or explicit migration notes.

## Start

1. Run `./scripts/bootstrap` from the repository root. Use `--output json` when
   a versioned capability receipt helps.
2. Inspect the current issue, PR, exact Git state, and overlapping work.
3. Read only the contract for the surface you will touch:

| Work | Read |
| --- | --- |
| Hostile/disposable execution | `docs/THREAT_MODEL.md`, then the applicable `docs/DISPOSABLE_*.md` |
| Residency/performance/cache | `docs/BLAZINGLY_HOT.md` and the owning issue |
| Workspace bootstrap | `docs/WORKSPACE_BOOTSTRAP.md` |
| Ownership, persistence, mutation, recovery, subprocesses, or physical experiments | `docs/AGENT_EXECUTION_SAFETY.md` |
| Delegation or cross-worker dependency | `docs/AGENT_COORDINATION.md` |
| Lease transitions | `docs/adr/0004-lease-lifecycle-core.md` |

Changing coordination state belongs in current GitHub issues, not this file.
Do not preload a broad documentation packet.

## Boundary

Glaeda is a Rust runtime for hot, trust-tiered Linux work on operator-controlled
compute. Trust selects one execution class:

- hostile/unknown: fresh isolated worker, one bounded job, exact teardown;
- trusted CI: prepared workers and reviewed reusable generations behind a clean
  job boundary;
- ultra-trusted project work: resident sandbox, task worktrees, build/package
  state, indexes, and selected services under an explicit lease and reset policy.

GitHub Actions remains the ordinary scheduler and workflow language. Glaeda owns
local admission, execution policy, identity, lifecycle, recovery, residency, and
measured execution decisions. It is not a new pipeline language, public
multi-tenant platform, custom hypervisor/runtime, generic cloud scheduler, or
automatic production deployment system.

## Always true

- Exact identity, bounded authority, fresh observation, crash recovery, and
  rebuildability form the correctness kernel.
- Resident state grants zero source, ownership, workflow-result, merge, cleanup,
  or mutation authority. Bind reuse to explicit trust, project, lease/generation,
  source, toolchain/prepared generation, capabilities, and reset policy.
- Names, paths, PIDs, tags, slots, cache hits, surviving services, and lease IDs
  do not prove ownership or exact release identity.
- Ambiguous continuity means revalidate, reset, quarantine, or rebuild. Never
  guess. Foreign, conflicting, and unknown resources stay protected.
- Performance observations recommend only. Compare complete agent loops, keep
  experimental levers separate, and preserve cold fallback.
- Unsafe Rust is forbidden. Use locked Cargo operations and commit `Cargo.lock`.
- Derive human and JSON output from one typed report. Never expose secrets,
  arbitrary logs, raw repository content, environment dumps, or private paths.
- Generated subprocesses use absolute programs plus argv, never `sh -c`; start
  child environments empty and add only allowlisted values.
- Planning must not mutate. Validate a complete mutation plan before its first
  effect. Roll back or compensate in reverse completion order; compensation is
  not restoration.
- Agent diagnosis, optimizer output, fleet directives, observations, and restored
  names grant zero authority. Reproduce a failure and observe the fix before
  calling it verified.

## Verify

Inner loop:

```bash
./scripts/verify fast
```

Complete test inventory when `cargo-nextest` exists:

```bash
./scripts/verify full-tests
```

Final code gate:

```bash
./scripts/bootstrap --output json
./scripts/verify required
```

The scripts own exact command order and output projection. `fast` and
`full-tests` are feedback, not publication authority. Add `--receipt PATH` for a
path-free, output-free performance receipt. Use `--output-mode stream` only when
live child output is needed. A bootstrap `blocked` result must be resolved;
`ready_with_declared_deviations` is acceptable only when each deviation is
recorded and irrelevant.
Use `./scripts/verify required --plan-json` when a machine-readable exact phase
and child-file-creation plan is needed. Do not copy that generated procedure.

Documentation-only changes may use the repository's docs-only policy. Record an
intentionally absent workflow run for ignored docs paths.

## Finish

- Finish the requested repository outcome in the active session or leave one
  exact blocked/failed continuation. Do not create polling schedules.
- Read the complete final diff. Verify current head, checks, authority, and
  provider state that can change the decision.
- Claim only evidence observed for the exact tested head and physical effect.
- Self-review is normal for low-risk reversible work. Privilege, ownership,
  adoption, persistence/recovery, rollback, secrets, concurrency, destructive
  behavior, and comparable high-risk changes need independent exact-head review.
- Ordinary repository writes and merges are allowed within task authority. Merge
  only the expected head after required checks and review. New head: review again.
- Human approval remains required for effects outside repository authority:
  credentials, access widening, material spend, operator-machine service or
  network mutation, releases/signing, external contact, destructive non-test
  data changes, and irreversible migration without recovery.

## Agent-facing writing

Write for the next agent. Short commands. Concrete nouns. One rule, one home.
Scripts and tests own mechanical truth; docs point instead of narrating it.
Return typed summaries with counts, omissions, and digests. Keep raw evidence
private and open it only for a named recovery need.
