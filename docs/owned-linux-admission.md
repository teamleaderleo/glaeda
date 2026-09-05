# Focused owned-Linux admission

The local `verify-focused run --admission-root <installed-private-root>` path adds a physical
launch gate to the existing verifier. An installed adapter supplies this root; a connected request
must never select, override, or omit it. The dispatch v2 focused capability forwards this fixed local
option. Ordinary local verification without the option retains its existing behavior.

The gate admits only `verify-focused/v1`: four CPUs, 8 GiB MemoryMax, and the existing fixed
TasksMax and deadline. It reserves one job in the installed root before source preparation and
retains that slot through physical settlement, task cleanup, and terminal receipt publication.
The slot is compute capacity state, not another work queue or execution/result identity.

Immediately around the existing `Popen`, the gate locks local operator policy, observes the host
through the pinned `glaeda-host-observe` binary, and calls the pinned `glaeda-local-admission`
reducer. It requires at least eight logical CPUs and available memory covering the 8 GiB profile
plus the configured owner reserve (minimum 4 GiB). CPU/memory/I/O PSI avg10 at or above
50%/1%/20% respectively asks the reducer to wait. Missing, malformed, stale, oversized or
unbound observations refuse. The combined observation/reduction must finish within three
seconds, including the final durable launch intent. Binaries are SHA256-bound and executed
through held file descriptors with a closed environment and bounded output.

Only the compatible `admit_now` decision is accepted, including all its false authority fields.
Caller/source/profile authorization remains in the existing adapters and verifier. This policy
is a coexist profile with no quiet-window claim. The new root is not an observer of an external
quiet-lease store, a scheduler of Windows work, a CPU affinity reservation, or a guarantee against
new unrelated host load. Existing quiet-window owners must integrate before claiming that gate.
No preemption, VM controls, network widening, or arbitrary commands are added.

## Operator control and installation boundary

Installation is a separate protected action; this change does not install a policy or service.
The installation must provide a private 0700 exact directory containing canonical UTF-8 JSON
`policy.json` (sorted keys, compact separators, newline; regular 0600 single-link file):

- `schema_version`: integer 1.
- `generation`: fresh random 64 lowercase hexadecimal characters identifying this installation.
- `revision`: positive integer, advanced by operator control.
- `node_control`: `held`, `draining`, or `available`; provision held until acceptance finishes.
- `memory_reserve_bytes`: integer at least 4294967296, chosen for the owner's reserved workload.
- `host_executable` and `policy_executable`: objects containing exact absolute `path` and
  `sha256` (`sha256:` plus 64 lowercase hexadecimal characters). Use the reviewed local
  `glaeda-host-observe` and `glaeda-local-admission` binaries, respectively.

Use the serialized control path, never edit policy behind an active launch transaction:

```bash
python3 scripts/owned-admission-control --root <installed-private-root> held
python3 scripts/owned-admission-control --root <installed-private-root> draining
python3 scripts/owned-admission-control --root <installed-private-root> available
```

Control refuses busy while the short check/spawn transaction is active. Retry after observing the
refusal; an unsuccessful control command has not established a hold. The slot lock spans the
whole job, but the policy lock does not, so a hold can prevent the next job while one is running.
Neither held nor draining stops an already-started job. Missing or changed installation policy
refuses new admission. Installation replacement must drain and settle the previous generation.

## Refusal and recovery

A hold/drain/pressure change during materialization is checked again before process creation.
A pre-launch refusal removes only that attempt's task and intent before releasing the slot. If
cleanup fails, the slot remains. Once process creation may have occurred, exceptions and crashes
retain the durable reservation. Dead PIDs, absent locks, age, and service restarts never authorize
redispatch. A failure between preparing the reservation and acquiring a terminal receipt remains
an explicit recovery case; no automatic stale-reservation deletion is provided.

Normal exact replay reads the existing receipt without entering admission or changing identity.
For recovery after terminal receipt publication but before slot release, call the same exact
verifier request with both `--reconcile-only` and `--admission-root`. Recovery validates the
existing receipt, matching reservation and installation generation, the digest binding the full
source/profile identity and exact command-state directory path/device/inode, then freshly observes exact unit
and task absence, and validates any remaining intent before releasing capacity. It does not run
source or recreate a result. Unsettled or mismatched state stays reserved.

## Evidence and next integration

`python3 scripts/test-owned-linux-admission.py` covers durable contention, crash refusal, exact
recovery, serialized control, a hold/drain/pressure change at the real child-launch boundary,
pre-launch cleanup, real disposable child settlement, immutable replay, filesystem substitution,
protocol binding, and bounded helper output. These are local child tests with fixture host facts;
they do not prove systemd/bubblewrap verification or a regular ChatGPT journey.

The next consumer change must pin this reviewed gate in the dispatch capability and resident
adapter, then prove named physical verification, capability revocation, restart recovery, two
requests, and timing. Service/capability installation requires its own concrete reviewed action.

## Pending-before-launch observation

`python3 scripts/owned-admission-observe --root <installed-private-root>` returns a bounded
`glaeda-owned-admission-observation` v1 JSON snapshot. It creates no lock, reservation, journal
or directory. All authority fields are false. A consumer may leave a request pending when
`outcome` is `wait`: `node_held`, `node_draining`, `pressure_high`, `capacity_unavailable`, or
`reserved`. A surviving reservation stays reserved; this observer never infers completion from
PIDs, age or lock availability. Invalid or unavailable observations return `refused` with
`observation_unavailable`, without exposing paths, host facts or exception text.

`ready` / `compatible` is only a disposable scheduling hint. The consumer must still validate
caller/source/profile/capability and the verifier must perform its fresh reservation and final
physical launch check. A race after this observation can still refuse at that boundary. The
observer does not schedule retries or publish a terminal result. Operator hold maps to pending
for the consumer; it remains a refusal in the physical reducer and launch path.
