# Glaeda

**Blazingly hot, trust-tiered execution for compute workloads — from one machine to a fleet.**

Glaeda finds the quickest trustworthy path from declared work to useful compute results while preserving exact ownership, recovery, and reusable state. Coding agents and GitHub Actions are major current workloads, while the runtime boundary is broader: CI, batch compute, data work, rendering, model workloads, and future typed adapters can use the same execution core.

> **Disposable is a capability. Trust decides residency.**

```text
work becomes known
-> identify exact workload, trust, inputs, and capabilities
-> select eligible compute and valid reusable state
-> admit capacity
-> execute
-> return bounded outputs and evidence
-> retain, reset, quarantine, migrate, or destroy physical state
-> recover from durable truth after interruption
```

See [`docs/COMPUTE_RUNTIME.md`](docs/COMPUTE_RUNTIME.md) for the product model and [`docs/BLAZINGLY_HOT.md`](docs/BLAZINGLY_HOT.md) for the hot-execution programme.

## Current status

Glaeda is pre-alpha and already exercises real systems paths on operator-owned Apple silicon. Current `main` contains the durable disposable-worker/control foundations, GitHub Runner Scale Set integration, prepared Lima/VZ worker support, trusted resident-state primitives, hot-state admission, task/source-view work, and bounded verification/performance receipts.

The current production direction still has active gaps around complete hostile-work network/credential hardening and end-to-end composition of the trusted resident task path. Current issues, pull requests, Git history, and [`docs/ROADMAP.md`](docs/ROADMAP.md) own progression; this README describes the present product boundary.

## Execution classes

| Trust class | Normal execution posture |
| --- | --- |
| Hostile / unknown | Fresh isolated worker, bounded capabilities, one workload, exact teardown and absence evidence. |
| Trusted repeatable compute | Prepared workers or warm pools may consume reviewed reusable generations while preserving a clean execution boundary. |
| Ultra-trusted resident compute | Long-lived project/compute state and selected services may remain resident under exact owner, generation, validity, capability, reset, and eviction rules. |

Resident state is acceleration and working state. It gains no independent source, ownership, result, merge, cleanup, or mutation authority by surviving. Durable decision state plus fresh observation drives recovery, and every hot-state family keeps a complete reset or cold-rebuild path.

Hostile-work details live in [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md). Ownership, recovery, mutation, subprocess, and physical-experiment rules live in [`docs/AGENT_EXECUTION_SAFETY.md`](docs/AGENT_EXECUTION_SAFETY.md).

## What Glaeda owns

Glaeda owns the compute-side layer shared across workload families:

- exact workload, attempt, ownership, and reusable-state identities;
- trust/capability and host-capacity admission;
- backend/host selection and lifecycle control;
- durable no-replay recovery across controller or machine interruption;
- hot-state validity, reuse, reset, retention, and teardown policy;
- bounded human/JSON evidence for results, recovery debt, and performance decisions.

Workload adapters own domain semantics and result authority. GitHub Actions remains the ordinary workflow scheduler, check/status surface, and hosted log owner for GitHub jobs. Backends such as Lima/VZ, native Linux, operator-owned fleet nodes, and future VM/container or burst providers remain mechanisms selected through capability and complete-loop evidence.

The first production host/backend is an operator-owned Apple-silicon Mac with Linux execution through Lima/VZ. macOS is the trusted control plane; filesystem-heavy workload execution happens inside Linux. See [`docs/LINUX-ACCEPTANCE.md`](docs/LINUX-ACCEPTANCE.md) and the threat model for the exact boundary.

## Commands

Inspect the current machine and plan repository work without applying it:

```bash
cargo run --locked -- --output json doctor
cargo run --locked -- plan --file examples/quarry.yml
cargo run --locked -- host plan --file examples/quarry.yml
```

`doctor` reports local readiness. `plan` validates a manifest and computes project actions. `host plan` adds host preparation decisions. Planning is read-only.

Host preparation is an explicit privileged path:

```bash
cargo build --locked
sudo ./target/debug/glaeda host prepare --file examples/quarry.yml
```

Review the exact built binary and plan before granting privilege. Repository code, scripts, and ordinary agent work never gain privilege merely because Glaeda can prepare a host.

Project manifests describe project identity and desired execution policy; they carry no executable shell surface. See [`docs/MANIFEST.md`](docs/MANIFEST.md) and [`examples/`](examples/) for the current schema and examples.

## Design principles

- **Exact identity before effects.** Names, paths, PIDs, tags, surviving processes, and cache presence are observations rather than ownership proof.
- **Fresh observation before mutation or release.** External state may change after any prior read or command response.
- **Durable truth survives physical loss.** VMs, caches, worktrees, disks, indexes, and resident services remain replaceable unless explicitly classified otherwise.
- **Trust decides reuse.** Hostile work stays fresh; trusted work may reuse reviewed state; ultra-trusted work may retain mutable state under explicit leases and reset policy.
- **Execution authority and result authority stay separate.** A successful process or reusable artifact carries only the meaning granted by its workload contract.
- **Cold fallback stays complete.** Hot-state failure, drift, or ambiguity must converge through reset, quarantine, rebuild, or explicit recovery debt.
- **Native mechanisms come first.** Glaeda composes mature operating-system, Git, filesystem, cgroup, service-manager, VM/container, and provider primitives instead of replacing them without measured need.
- **Measure complete useful-result paths.** Performance claims bind exact comparable inputs, validation, resource cost, and fallback behavior.

## Development

Glaeda uses Rust 2024 on stable Rust and commits `Cargo.lock`.

```bash
./scripts/bootstrap
./scripts/verify fast
./scripts/verify full-tests
./scripts/verify required
```

`fast` is the compact inner loop, `full-tests` runs the complete test suite, and `required` is the repository-required final verification profile for code changes. The verification helper also supports `--plan-json` and bounded receipts.

Documentation-only changes follow the repository's docs-only verification policy; `.github/workflows/ci.yml` intentionally ignores Markdown and `docs/**` changes.

## Project documents

- [`docs/COMPUTE_RUNTIME.md`](docs/COMPUTE_RUNTIME.md) — general compute-runtime boundary and typed workload seam.
- [`docs/BLAZINGLY_HOT.md`](docs/BLAZINGLY_HOT.md) — trusted residency and hot execution.
- [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) — hostile/unknown execution boundary.
- [`docs/AGENT_EXECUTION_SAFETY.md`](docs/AGENT_EXECUTION_SAFETY.md) — ownership, durable state, mutation, subprocess, and experiment safety.
- [`docs/WORKSPACE_BOOTSTRAP.md`](docs/WORKSPACE_BOOTSTRAP.md) — repository bootstrap contract.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — current sequencing and detailed programme state.
- [`docs/MANIFEST.md`](docs/MANIFEST.md) — project manifest model.
- [`AGENTS.md`](AGENTS.md) — repository instructions and task routing for agents.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
