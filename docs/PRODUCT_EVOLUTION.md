# Product evolution and scope discipline

SmolRunner began with a small practical irritation: use the operator's own Mac for expensive build and test work instead of waiting on hosted GitHub Actions runners.

The project has grown far beyond a shell wrapper around Lima. That growth is deliberate. Each major expansion answers a problem exposed by making the previous step dependable enough to use.

## The dependency chain

The product direction can be read as one sequence:

1. **Use local compute.** A Mac already has useful CPU, memory, and persistent storage, so routine verification should be able to run there.
2. **Make the local worker reproducible.** Hand-maintained shell setup and pet VMs drift, break, and require operator memory. The worker needs reviewed inputs, explicit resource profiles, typed status, and repeatable lifecycle actions.
3. **Make executions disposable.** A persistent worker is convenient until repository code, dependencies, services, or build state become hostile or simply dirty. One fresh VM per job gives each execution a clear lifetime and cleanup boundary.
4. **Make disposal crash-safe.** Once workers are created and destroyed automatically, controller death can happen between any two external side effects. Durable attempt identity, checkpoints, recovery, and exact ownership become requirements for avoiding duplicate workers, lost jobs, or unsafe cleanup.
5. **Use GitHub as the normal interface.** The useful operator experience is ordinary Actions submission and status, so Scale Set demand, one-time JIT registration, exact job binding, and runner removal need to join the durable local lifecycle.
6. **Treat repository code as hostile.** A disposable VM limits persistence, but production use also needs credential containment, controlled network access, hard resource ceilings, and recovery from malicious dependencies or compromised jobs.
7. **Make the service unattended.** Autoscaling becomes worthwhile when queueing, sleep/wake, reboot, stale registrations, failed provisioning, stuck jobs, retries, teardown, and scale-to-zero converge without routine operator work.
8. **Recover warm-runner speed.** Disposable execution throws useful state away with dangerous state. Prepared templates, reviewed dependency inputs, compiler caches, container caches, cache trust classes, quotas, and benchmark-driven tuning recover performance while preserving cheap compromise recovery.
9. **Expose a build-farm experience to agents.** Once the preceding guarantees hold, trusted agents should simply submit work, observe queue/result state, and continue. The operator should rarely think about the worker service.

This sequence is the rationale behind the M1-M7 roadmap. The milestones describe capabilities that depend on one another rather than independent feature accumulation.

## A test for scope growth

A proposed addition belongs on the critical path when it satisfies most of these conditions:

- it removes a demonstrated blocker to the single-Mac disposable-worker product;
- it closes a realistic crash, escape, persistence, secret, lateral-movement, resource-exhaustion, cleanup, or operator-burden failure mode;
- it makes the ordinary GitHub/agent journey simpler after the underlying safety work exists;
- it improves measured queue-to-result performance without weakening compromise recovery;
- it reuses mature components and keeps SmolRunner focused on admission, identity, policy, recovery, and Mac-specific coordination;
- it can be expressed as a bounded capability with explicit evidence and acceptance criteria.

An idea belongs in an adjacent or deferred lane when it can succeed independently of the single-Mac unattended lifecycle, introduces a second scheduler or workflow language, widens host authority for convenience, or creates a subsystem before measurements show the mature alternatives are insufficient.

That distinction is why project namespaces, blank-Mac recovery, richer terminal integration, multi-host/provider selection, previews, deployment, dashboards, and broad fleet management can be valuable without becoming prerequisites for the current disposable-worker path.

## The product center

The project should keep returning to the same operator promise:

> Expensive GitHub work should use the operator-owned Mac automatically, quickly, and safely, while each execution remains disposable and routine failures recover without babysitting.

The roadmap may grow as hidden requirements become visible. The product center stays fixed.