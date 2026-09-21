# Reusable-state lifecycle

This document defines the evidence contract behind issue #21's cache lifecycle. It is a policy and
observation layer. Physical cache stores, repository validators, execution records, and canonical
artifacts keep their existing owners.

The rule is simple: expensive reusable state earns retention through exact identity, trusted
publication, successful read-only consumers, and measured net value. A directory name or continued
presence on disk carries zero reuse authority.

## State families stay distinct

Glaeda classifies reusable acceleration before applying lifecycle policy:

| class | typical producer | validity emphasis | ordinary consumption |
| --- | --- | --- | --- |
| `package_manager_state` | package-manager/native cache client | repository, platform, runtime, lock digest, cache schema | read-only when trust policy permits |
| `compiler_cache` | reviewed compiler-cache publisher | repository, architecture, compiler/toolchain, flags, build configuration, schema | read-only |
| `incremental_build_state` | project-local reviewed build producer | project/source family, architecture, toolchain, build configuration, flags, project generation | project scoped |
| `immutable_compiled_product` | successful reviewed compile + product validator | source/build identity, Xcode/SDK/runtime, product contract, archive digest, object schema | immutable read-only |
| `container_layer` | reviewed BuildKit/OCI producer | build inputs, runtime/platform, layer/config digests, schema | immutable/read-only |
| `prepared_dependency_generation` | reviewed environment builder + validator | lock digest, runtime/toolchain, platform, preparation generation, schema | immutable/read-only |
| `project_local_approved_hot_state` | ultra-trusted project lifecycle owner | project lease, source/task class, toolchain/prepared generation, policy generation | project-scoped read/write under its own owner |

The lifecycle model never turns these into one shared writable directory. Family-specific owners
still decide exact semantic inputs, validation, physical sharing mode, locking, and reset behavior.
`family_inputs_digest` binds any validity input that does not have a common typed field.

## Versioned identity

`ReusableStateIdentityContract` includes:

- state class and schema version;
- canonical repository identity;
- architecture;
- toolchain generation;
- OS/runtime generation;
- optional dependency-lock digest;
- optional build-configuration digest;
- optional compiler-flags digest;
- optional prepared-environment generation;
- cache-schema generation;
- a family-specific exact-input digest.

Every field participates in the canonical SHA-256 identity digest. Mutable labels such as `main`,
`latest`, and `current` have no place in the authority contract.

For cmux compiled products this composes with #13363/#13364/#13384: provider/artifact identity,
archive digest, product receipt/fingerprint, source/build identity, and object-format generation stay
inside the compiled-product family's exact-input binding. Node-local residency never replaces the
inner product receipt.

## Consumers and publishers

Two publisher roles exist:

```text
consumer_only
reviewed_trusted_publisher
```

Only `reviewed_trusted_publisher` can construct a candidate generation. A job that happened to
write files has no publication authority.

Consumption is always reported as `read_only` by this layer. Trusted consumers may exercise a
validated candidate to build consumer evidence. Lower-trust consumers can receive only a preferred
generation, and only when the family policy explicitly permits low-trust read-only consumption.

## Lifecycle

The accepted progression is:

```text
candidate
  → validated
  → observed_consumers
  → preferred
  → demoted
  → retired
```

A candidate can become validated after a complete verified publication. `observed_consumers`
requires a successful consumer and zero semantic mismatches. `preferred` requires the full promotion
policy, including at least two producer successes, successful consumers, measured positive net time
saved, zero semantic mismatch, and an acceptable reset/validation-failure rate.

The default conservative policy currently requires:

```text
producer successes:       >= 2
successful consumers:     >= 2
lookups for value:         >= 3
net time saved:            > 0 ms
validation failures:       0
reset/invalidations:       <= 10 / 1000 lookups
```

These thresholds are policy inputs, not semantic validity. Family validators retain semantic
ownership.

## Bounded generation metrics

Each generation record carries only low-cardinality public accounting:

```text
lookups
hits
misses
restore_duration_millis
publication_duration_millis
bytes_read
bytes_written
storage_size_bytes
estimated_cold_work_millis
estimated_warm_work_millis
last_useful_hit_epoch_millis
validation_failures
reset_invalidation_count
reset_invalidation_overhead_millis
producer_successes
successful_consumers
semantic_mismatches
```

The reusable-state record has no field for paths, source contents, credentials, environment values,
command output, or arbitrary logs.

## Utility

Utility is explicit and inspectable:

```text
saved_per_hit = max(estimated_cold_work - estimated_warm_work, 0)
avoided_work = hits * saved_per_hit

net_time_saved
  = avoided_work
  - restore_duration
  - publication_duration
  - reset/invalidation overhead
```

Storage size remains separate. This prevents a small timing gain from silently erasing the disk cost
of a huge generation.

The policy surface returns bounded recommendations:

```text
observe
promote
retain
revalidate
demote
retire
evict
stop_publishing
```

A generation with enough observations and zero/negative measured net time gets `stop_publishing`
before it can become preferred.

## Automatic supersession cleanup

Supersession is explicit family evidence, never a guess from age, names, paths, source proximity, or
directory presence.

A family owner can state:

```text
exact generation B supersedes exact generation A
```

The shared planner accepts that edge only when B is already a healthy `preferred` generation with
complete verified publication and no revalidation debt. It then classifies A:

```text
reconstructible + no consumers + no must-retain/unique work
  -> automatic_cleanup

reconstructible + active consumers
  -> deferred_in_use

unique local work
  -> preserve_unique_local_work

must-retain state
  -> preserve_must_retain

non-reconstructible state
  -> preserve_non_reconstructible
```

A normal family reconciliation loop can run this on every meaningful generation/lease transition.
No operator action is required for a safe superseded generation: once the final consumer lease
releases, the next reconciliation selects the complete predecessor generation for retirement.

The generic layer still performs no filesystem mutation. Family executors consume
`automatic_cleanup` selections and must freshly re-check the exact object identity and every
consumer/transfer lease before atomic whole-generation retirement. #914/#965 are the current
physical precedent: no-replace rename, durable retirement debt, incremental no-follow deletion, and
crash recovery.

There is deliberately no generic inference that "newer" means "supersedes." Families that want
automatic replacement cleanup must provide the exact predecessor/successor edge.

## Disk budget and eviction

`plan_reusable_state_eviction` is a pure whole-generation planner. It performs no deletion.

A generation enters automatic reclamation only when all of these are true:

```text
reconstructible: true
unique_local_work: false
must_retain: false
in_use_consumers: 0
```

Pressure starts at the configured high watermark and aims for the low watermark. Selection is
caller-budgeted per reconciliation pass and deterministic: retired/demoted generations first, then
lower net time saved, older useful hits, larger reclaimable size, and generation identity. The
shared policy has no fixed maximum generation count or maximum selection count; a caller can choose
a small per-pass work budget and resume on the next reconciliation without imposing a total catalog
ceiling.

This composes with #926's already-merged physical Cargo-target retirement path, which owns locked
rename/delete, lease fencing, crash recovery, and its 90%/85% host-pressure hysteresis. The generic
planner does not gain filesystem authority.

Canonical execution truth cannot be an automatic-eviction candidate. Unique local work and
mandatory state are retained. An in-use generation is skipped whole; cached contents are never
mutated in place to make space.

## Corruption and invalidation

Consumption resolves every broken or ambiguous state as a miss/reset. Covered classes include:

- truncated generation;
- exact-identity mismatch;
- toolchain-generation mismatch;
- dependency-lock change;
- partial publication;
- disk-full publication;
- producer crash;
- concurrent-publisher conflict;
- invalidated preferred generation;
- consumer crash while an in-use lease remains visible.

The consumer-crash rule is intentionally asymmetric: generation bytes remain immutable, and the
visible in-use reference blocks reclamation until the family owner reconciles the lease.

## Agent-readable status

`ReusableStateStatusSummary` is designed for #970/#546-style advisory status. It exposes only:

```text
cache_class
generation
heat: cold | warm | hot
size: empty | tiny | small | medium | large | huge
recent_hit: never | within_hour | within_day | within_week | older | clock_skew
revalidation_required
```

No path or cached content appears in the type. `hot` currently means a preferred generation with
positive measured utility and a useful hit within the previous day. These summaries are hints for
routing and planning; they carry no execution, verification, or publication authority.

## First proving observations

### Glaeda Rust/Cargo incremental state — retain

Issue #926 measured an exact native Rust workload on Rust/Cargo 1.97.1 with four Cargo jobs:

```text
cold median: 43.985 s
warm median:  3.355 s
retained target: about 1.895 GB allocated
cold→warm saving: 40.630 s / 92.37% / 13.11x
```

Repeated accepted consumers reused the preceding target generation. Under the lifecycle contract,
repeated successful consumers plus the large positive net value make this state a `retain` candidate
once its producer identity and ordinary family validity checks are satisfied.

A smaller dependency-free Rust discriminator from the same issue measured 118.094 ms cold versus
24.564 ms warm while retaining 8,130,560 allocated bytes, providing a second scale point for the
same class.

### cmux/Xcode compiler-result cache — stop publishing for the observed mode

The native Apple experiment attached to #1048 saw three compiler-cache hits across reverts/branch
returns, while full checks with compilation-result caching took roughly 65–76 seconds compared with
about 51–55 seconds for the hashing-only observation path. The Glaeda lane consequently kept this
cache opt-in and disabled it in the everyday profile.

That maps to `stop_publishing`: hits alone do not earn promotion when the complete measured path has
zero positive avoided work. A real generation record must still include its measured storage size;
the policy decision above uses the observed timing discriminator before storage cost is considered.

The cmux node-local immutable app-host product is a separate `immutable_compiled_product` class.
#13364's current canary object is 606,055,512 bytes, and #13384 requires exact provider/archive/
product identity, verified publication, read-only consumers, in-use leases, and whole-object budget
reclamation. Its retain/evict verdict stays `observe` until the node-local lane records restore/fill
cost and repeated useful hits.

### npm package state — revalidate after incomplete warm state

The Big Red Node/npm observation recorded a nominally warm cache whose strict offline `npm ci`
failed in 0.909315 seconds because `zip-dir@2.0.0` was missing. Prefer-offline reconstruction then
took 4.308888 seconds and produced a 191,680,512-byte / 9,865-entry dependency tree.

That maps to a miss/reset plus `revalidation_required`. Presence of the cache directory cannot claim
offline completeness or promotion. A future package-cache generation must bind the lock/runtime/
platform identity and record complete validation before preferred consumption.

## Integration boundaries

- **#546 adaptive routing:** consume utility/recommendation/status summaries as advisory inputs. It
  cannot reinterpret them as execution authority.
- **#547 adaptive verification compiler:** supply family semantic inputs, builders, validators, and
  exact derived-artifact contracts. The lifecycle records measured reuse value after validation.
- **#557 / #560 hot project state:** keep approved project-local mutable state under its project/
  lease owner. Cross-trust publication requires a separately validated generation; residency alone
  cannot promote it.
- **#970 agent-readable status:** surface only the small status summary above. Remote copies are
  advisory and can require revalidation.
- **#926 protected Cargo retirement:** keep physical locking, in-use fencing, crash recovery, and
  deletion authority there. The generic lifecycle can provide value ordering without bypassing that
  producer contract.
- **cmux #13363/#13364/#13384:** preserve compiled-product eligibility, transport, and node-local
  immutable residency as three separate responsibilities.

