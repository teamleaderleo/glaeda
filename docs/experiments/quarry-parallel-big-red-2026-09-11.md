# Quarry parallel-full: Big Red triangulated batch result

Status: experiment adapter landed as `scripts/quarry_parallel_impl.py` with
unit gate `scripts/test-quarry-parallel.py` (20 tests), plus a bounded
channel extension to the shared task primitive
(`execute_capturing`/`execute_capturing_split` in
`scripts/owned_linux_task.py`). The measurements are performance
observations, not source-validity, publication, merge, or result-reuse
authority.

## Landed slice

One Glaeda-managed batch runs Quarry's exact repository-owned
`parallel-full` verifier — complete corpus, internal sharding, canonical
v2 `--receipt-stdout` channel — inside the shared task-private
systemd/bubblewrap boundary (network `none`, closed environment,
absolute argv only) and returns one bounded canonical receipt
(`glaeda-quarry-parallel-receipt`, stdout only).

```text
exact resident Quarry commit/tree + clean checkout + observed pytest closure
-> experiment admission gate (pressure, memory/CPU reserve, VPN lane, RDP observed)
-> pristine task-private materialization from resident Git
-> writable task clone (full object copy, revalidated commit/tree/clean)
   + origin/main ref fetch (the verifier observes it; a detached clone lacks it)
   + task-local .git/info/exclude for the build mountpoint (never committed)
-> fixed venv skeleton (/venv: pyvenv.cfg + symlinks + ro closure bind)
-> systemd-run grant: CPUQuota=400%, MemoryHigh=6G, MemoryMax=8G,
   TasksMax=512, RuntimeMaxSec=900, network none
-> fixed argv: task-python scripts/run_local_tests.py parallel-full
   --workers 4 --receipt-stdout
-> split-stream bounded capture (stdout receipt <= 65,536; stderr private digest)
-> unit settlement, task cleanup
-> one canonical receipt: source, toolchain, workload, grant, admission
   before/after, per-shard states + output digests, outer terminal/cleanup
```

No queue, daemon, scheduler, remote CLI, or caller-selected command,
mount, or resource property. Quarry owns the verifier and its receipt
semantics; Glaeda owns admission, isolation, bounded capture, cleanup
observation, and the outer receipt.

## Admission: VPN lanes and interactive RDP are never starved

Same gate as the sweep slice: refuses before any materialization when
the Tailscale lane is not `IFF_UP`, PSI `avg10` exceeds cpu 30 / mem 10 /
io 30, available memory is below the 8 GiB job+owner reserve, or fewer
than 6 CPUs are visible. The job carries no network traffic and is
capped at 4 of 16 CPUs / 8 GiB of 30 GiB. RDP (`:3389` LISTEN) is
observed and recorded before/after every attempt rather than required.

## Complete comparable loop

Exact workload: Quarry `74eb66ee` / tree `6cecb3f9`, workers 4,
`--receipt-stdout`, pytest 9.1.1 on CPython 3.14.4, closure
`sha256:6dd40c9d…` (full value in the receipt).

| Arm | Result | Wall | Worker-visible bytes | Notes |
| --- | --- | ---: | ---: | --- |
| Cold direct (n=2) | completed, 15/15 passed | 86.1 s / 85.4 s | 8,049 receipt | resident venv, host /tmp |
| Glaeda managed (n=3) | shard_failure (see below) | 64.7–71.2 s | ~7,000 receipt + ~28k stderr digest | task boundary, 400%/8G grant |

No speedup is claimed. The managed wall is shorter here only because
failing shards cancel their siblings early — comparing a failed run
against a passed run proves nothing about speed. The product win is
admission, isolation, and bounded identity-bearing evidence for an
external workload that previously had none.

## What the failures taught (the actual findings)

Three sandbox-shaped failures, each diagnosed to a root cause:

1. **Read-only source vs worktree creation.** `parallel-full` creates
   git worktrees inside its checkout. Fix: writable task clone (full
   object copy, `--no-hardlinks`, revalidated) bound writable; canonical
   resident state is never mounted and shares no inodes. This is the
   physical meaning of #1011's "task-private checkout" requirement.

2. **Toolchain probe requires a venv interpreter.** Quarry's
   `capture_verifier_toolchain` probes `sys.executable -I`, which only
   sees pytest when the launcher itself runs as a venv interpreter.
   Fix: fixed task venv skeleton (`pyvenv.cfg` + symlinks + read-only
   resident closure bind), with `quarry.__file__` precedence proven to
   resolve to the task source, not the venv's editable install.

3. **Detached clones lack the observed `origin/main` ref.** The
   verifier's repository observation reads `refs/remotes/origin/main`
   (falling back to `refs/heads/main`); the continuation test failed
   deterministically 2/2 managed vs 2/2 cold passing plus 50/50
   standalone passes. Fix: fetch exactly that ref from the admitted
   resident checkout into the task clone and verify equality. Bisected
   through namespace isolation (not network, not uid, not tmpfs) down
   to the exact `git rev-parse` error text.

4. **Tmpfs sizing is workload-shaped.** Fifteen materialized shard
   worktrees do not fit the shared 512 MiB `/tmp` tmpfs (honest
   `ENOSPC` with truthful started/cancelled/not-started states — the
   receipt's cancellation semantics working as designed). Fix:
   workload-scoped 2 GiB `/quarry-tmp` tmpfs; the shared constant is
   untouched.

## Open gap: the accepted toolchain closure

One shard (`verifier`, 10 tests) fails only in the sandbox: nested
isolated-verifier subprocesses misbehave under the inferred venv
skeleton, leak a generated `tests/test_ambient.py` into the worktree,
and the outer collection then correctly refuses the ambient plugin
declaration. The sandbox faithfully executes 3066/3067 tests with
truthful receipts; the failure is real behavior divergence, not harness
noise.

The right fix is not more sandbox tricks. Quarry's own
`verification_toolchain.py` owns toolchain identity, and #1011 calls
for an "accepted Quarry verifier/toolchain generation". The closure
must be defined with the quarry lane against that contract (what makes
a rebased Python closure the accepted generation, and what identity it
carries), not inferred by the executor. Until then, this adapter is an
experiment harness with a known one-shard divergence, and its receipts
must not feed the Rust adapter's success path.

## What this does not do

- No Rust-adapter integration: receipts stay in the Python experiment
  boundary. Feeding `quarry_parallel_verification_adapter` requires the
  accepted closure above plus the personal-worker attempt machinery
  (reservations, bindings, generations) that this slice deliberately
  does not invent.
- No durable replay store: each attempt executes fresh under a
  workload-identity unit name that refuses on collision.
- No production admission-root installation or machine mutation.

## Proposed next workload

Close the toolchain-closure question with the quarry lane (their
`verification_toolchain.py` is the owning contract), then bind one
managed receipt to one Glaeda attempt through the already-landed Rust
adapter — that composition is the missing #1011 physical producer, and
every piece except the accepted closure is now proven on Big Red.
