# Agent execution safety

Read this before changing ownership, adoption, durable persistence, mutation,
recovery, subprocess, host, or physical-experiment behavior. `AGENTS.md` owns the
universal entry rules. This file owns the cold implementation detail.

## Ownership and adoption

- Production planning and probes use kind-specific canonical resource
  constructors. Do not invent free-form locators or fingerprints.
- Desired identities require their kind's minimum immutable evidence. An
  observation may omit evidence only so classification can return `unknown`;
  present evidence must validate canonically.
- An unmarked exact-evidence match requires explicit adoption confirmation. It
  never becomes managed automatically.
- A lease ID alone proves no ownership of a container, workspace, route, disk,
  cache, artifact, or resident project sandbox.
- Follow `docs/adr/0004-lease-lifecycle-core.md`. Accepted lease transitions
  advance the revision. Terminal leases stay terminal.
- Restores begin quarantined. Adopt restored names or documents only after fresh
  host, GitHub, and backend evidence satisfies the canonical ownership contract.

## Durable state

Do not write authority-bearing or decision state until that state family has:

- atomic persistence and locking;
- restrictive permissions and symlink defense;
- crash recovery and migrations;
- installation/generation identity.

Cheap performance hints may skip that machinery only when bounded,
privacy-preserving, non-authoritative, disposable, and irrelevant to every safe
mutation/reuse decision. If a hint changes what is safe, it is decision state.

Exact-input mismatch, corrupt reusable state, an invalid project lease, or a
stale toolchain generation takes the miss/reset/cold path. Never reuse
optimistically.

Keep project-local mutable state separate across trust/project boundaries.
Shared writable state needs an explicit poisoning, ownership, quota, and
publication design. Public journals contain only public receipts and failures.

## Mutation and recovery

- Do not add an apply path until ownership persistence, root elevation,
  runner-user execution, journal persistence, credential acquisition, and
  package rollback are implemented for it.
- Every host mutation needs plan/dry-run behavior and a defined rollback or
  compensation class. Invalid or unconfirmed irreversible work blocks the whole
  batch before the first executor call.
- No automatic repair without an explicit action-class policy, budget, circuit
  breaker, exact owner, durable checkpoint, fresh post-action observation, and
  rollback/compensation class.
- Never replace privileged control logic during an active job or reconciliation
  journal. Require exact binary digest, compatible state, drain, a previous
  verified version, and post-switch health evidence.
- Fleet desired state and optimization recommendations remain inputs. Local
  ownership conflicts, unknown state, active recovery/work, and operator holds
  veto them.

## Process and dependency safety

- Child programs use absolute executable paths and argument vectors. No implicit
  shell.
- Child environments start empty and receive only allowlisted values.
- Output redaction is defense in depth, not proof that a child cannot transform
  or leak a secret.
- Prefer stable system tools over recreating package-manager, filesystem,
  systemd, Git, container, proxy, or TLS behavior.
- Keep Linux code behind a narrow host/backend boundary. Unsupported platforms
  fail clearly.
- Add dependencies only for a concrete need and maintenance rationale. Pin
  third-party GitHub Actions to reviewed commit SHAs.
- Tests require no root, systemd, Podman, formatting, VM creation, reverse proxy,
  or live credentials unless explicitly marked physical/integration with exact
  opt-in.
- Manifests describe host/execution policy. Language build behavior stays in
  repository scripts, package-manager files, Containerfiles, and workflows.

## Performance changes

A performance claim names:

- baseline and exact candidate;
- comparable-work definition and semantic validator;
- primary latency metric;
- secondary CPU, RAM, and disk effects;
- fallback/reset behavior.

Builders and benchmark workloads cannot grant stronger cache, artifact, or
residency authority by emitting metadata. Incident/performance evidence stays
local-first, bounded, versioned, and content-minimised.

## Physical experiments

Before a hot-execution experiment, record exact Glaeda head, hardware/OS class,
backend, guest/kernel/filesystem, project/revision, toolchain/package manager,
resource profile, and candidate. Then:

1. hold one semantic workload constant while changing one optimization;
2. separate cold and warm/reuse paths;
3. separate physical allocated bytes and host backing growth from logical size;
4. retain exact cleanup/rebuild evidence for experiment-created resources;
5. quarantine ambiguous physical state; do not sweep broadly;
6. publish only bounded results and opaque/canonical identities.

Do not format filesystems, create VDO state, replace VMs, delete disks, or mutate
operator-machine networking as ordinary hosted CI.
