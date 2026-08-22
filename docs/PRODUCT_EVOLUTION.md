# Product evolution and scope discipline

SmolRunner began with a small practical irritation: use the operator's own Mac for expensive build and test work instead of waiting on hosted GitHub Actions runners.

The project has grown far beyond a shell wrapper around Lima. That growth is deliberate. Each major expansion answers a problem exposed by making the previous capability dependable enough to use.

The product center is now sharper:

> **SmolRunner should be blazingly hot. Disposable is a capability. Trust decides residency.**

The optimization target is agent wall-clock latency: work becomes known, a useful execution environment is already available or materializes quickly, reusable state is ready, and the first useful command starts with as little repeated work as possible.

## The dependency chain

The product direction can be read as one sequence:

1. **Use local compute.** A Mac already has useful CPU, memory, and persistent storage, so routine verification should be able to run there.
2. **Make the local worker reproducible.** Hand-maintained shell setup and pet VMs drift, break, and require operator memory. The worker needs reviewed inputs, explicit resource profiles, typed status, and repeatable lifecycle actions.
3. **Build a strict disposable execution capability.** Hostile and unknown repository work needs one fresh isolated worker, one bounded job, exact cleanup, and proven absence.
4. **Make disposal crash-safe.** Once workers are created and destroyed automatically, controller death can happen between any two external side effects. Durable attempt identity, checkpoints, recovery, and exact ownership become requirements for avoiding duplicate workers, lost jobs, or unsafe cleanup.
5. **Use GitHub as the normal interface.** The useful operator experience is ordinary Actions submission and status, so Scale Set demand, one-time JIT registration, exact job binding, and runner removal need to join the durable local lifecycle.
6. **Treat repository code according to trust.** Hostile work gets the strict disposable contract. Trusted CI may consume prepared images, reviewed caches, repository seeds, and warm pools. Ultra-trusted agent work may retain project-local mutable state when exact validity and reset rules allow it.
7. **Make the service unattended.** Autoscaling becomes worthwhile when queueing, sleep/wake, reboot, stale registrations, failed provisioning, stuck jobs, retries, teardown, and scale-to-zero converge without routine operator work.
8. **Make every reusable layer hot.** Prepared environments, resident project sandboxes, repository objects, dependency state, compiler outputs, language indexes, test plans, derived artifacts, and selected services can stay ready according to trust and measured value.
9. **Optimize complete agent loops.** The key measurements become queue-to-first-command, edit-to-first-test-result, edit-to-final-relevant-verification, total task wall time, and fleet throughput under concurrency.
10. **Expose an execution substrate to agent fleets.** Trusted agents should submit work into an environment that has already performed every reusable, trustworthy piece of preparation it could do before the task arrived.

This sequence is the rationale behind the roadmap. The early milestones establish durable execution truth and the strict hostile-work lane. The hot-execution programme then uses that durable kernel to make persistence aggressive where trust permits it and cheap to discard when validity changes.

## Trust-tiered residency

SmolRunner has three useful execution classes.

### Hostile / unknown

```text
fresh isolated worker
-> one bounded job
-> teardown
-> prove absence
```

Reusable inputs are separately reviewed and immutable or read-only. Job-specific writable state disappears with the worker.

### Trusted CI

```text
job arrives
-> select prepared worker or warm pool
-> attach exact repo/source state
-> attach eligible cache/artifact generations
-> execute
-> retire or destroy according to policy
```

The individual worker can remain disposable while preparation, repository objects, compiler state, and reviewed cache generations stay hot around it.

### Ultra-trusted agent/project work

```text
project sandbox is already resident
-> sync/revalidate intended source state
-> continue incremental build/index/tool state
-> edit/test/build
-> retain useful project state for the next iteration
```

A resident sandbox is working state. Canonical source identity, durable execution truth, trust class, project lease, toolchain generation, credential/network capability generation, and reset policy remain authoritative.

## Destroyability enables aggressive residency

The core correctness property is **destroyability**.

SmolRunner should be able to discard a VM, workspace, project sandbox, cache, index, prepared materialization, or compiler tree and still know what happened, what remains owned, and how to reconstruct the next safe action.

That property makes persistence powerful. Valuable trusted state can stay resident for hours or days because losing it costs latency and compute instead of execution truth.

Useful hot-state classes include:

- **immutable reusable generations** — prepared images, dependency environments, repository seeds, derived datasets, shard plans;
- **resident project state** — checkout/worktree state, incremental build trees, language indexes, dev databases, project-local services;
- **task-local state** — uncommitted edits, debugging state, temporary outputs;
- **disposable physical caches** — package caches, compiler caches, indexes, downloaded immutable blobs.

Each class gets its own lifetime, trust, validation, quota, and eviction rules.

## A test for scope growth

A proposed addition belongs on the main product path when it satisfies most of these conditions:

- it removes a demonstrated blocker to dependable local execution;
- it closes a realistic crash, escape, persistence, secret, lateral-movement, resource-exhaustion, cleanup, or operator-burden failure mode;
- it reduces measured queue/edit-to-useful-result latency;
- it preserves or improves fleet throughput under concurrent agent work;
- it makes exact reusable work available earlier through residency, precomputation, caching, or overlapping preparation;
- it reuses mature components and keeps SmolRunner focused on admission, identity, policy, recovery, hot-state lifecycle, and Mac-specific coordination;
- it can be expressed as a bounded capability with explicit evidence and acceptance criteria.

Adjacent ideas remain useful when they can succeed independently of the current execution loop or introduce broad product surface before measurements justify it.

## Optimization hierarchy

For a representative trusted workload, prefer optimizations in this order unless measurements point elsewhere:

1. retain valuable trusted state;
2. remove repeated semantic work;
3. reuse exact completed work;
4. overlap independent preparation;
5. share immutable inputs;
6. parallelize genuinely independent work;
7. optimize hot kernels and storage behavior;
8. add more hardware or paid burst capacity.

This keeps SmolRunner focused on deleting latency before buying around it.

## The product center

The project should keep returning to the same promise:

> **Agent work should arrive to a Linux execution environment that feels ready before the task asks for it. Hostile work gets fresh disposable isolation. Trusted work gets aggressive residency, incremental state, and reuse. Durable execution truth survives either choice.**

The Mac is the control plane and owned compute source. Linux is the execution environment. GitHub remains the ordinary workflow surface. SmolRunner makes the path between them hot, recoverable, inspectable, and increasingly anticipatory.
