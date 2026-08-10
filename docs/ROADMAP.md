# Roadmap

SmolRunner's primary outcome is unattended, disposable GitHub Actions capacity on an operator-owned Mac. The governing boundary and acceptance criteria are in [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md).

GitHub Actions remains the scheduler and workflow language. The first complete backend is one fresh Lima/VZ virtual machine and one just-in-time official runner per job. Rootless host containers, previews, deployment, and multi-host placement are not on the critical path.

## Useful foundation already built

- [x] Rust CLI with shared typed human/JSON reports.
- [x] Canonical configuration, host observations, ownership classifications, and public error vocabulary.
- [x] Bounded shell-free process execution with an empty explicit environment.
- [x] Crash-safe durable stores, atomic journals, recovery classification, revisions, and queue generations.
- [x] Typed personal-worker queue, admission, reservations, resource limits, cancellation, and terminal identities.
- [x] Mac capacity observation and Lima observation/lifecycle authority.
- [x] GitHub workflow-job mapping and snapshot reconciliation foundations.
- [x] Extensive optional Linux/rootless-Podman R01 prerequisite and closure evidence.

The R01 implementation is preserved, but no additional narrow OS/runtime proof slice is scheduled before the disposable VM path works end to end.

## Milestone 1 — disposable-attempt reconciliation

- [x] Define one durable identity joining a scale-set capacity claim, reservation, VM, runner, actual assigned GitHub job, and attempt.
- [x] Define a small crash-recoverable phase graph from admission through cleanup and release.
- [x] Emit exactly one idempotent next action per reconciliation tick.
- [x] Enforce global concurrency, memory, CPU, and disk budgets before provisioning.
- [ ] Cover cancellation, expiry, runner loss, orphan VM, stale registration, every checkpoint interruption, bounded retry, and scale-to-zero in deterministic tests.

## Milestone 2 — disposable Lima/VZ worker

- [ ] Pin a reviewed Apple-silicon Ubuntu template and official runner image contents.
- [ ] Create, start, observe, and destroy a unique one-job VM through bounded Lima commands.
- [ ] Use Lima plain mode or an equivalent exact configuration with no host mounts, SSH-agent forwarding, dynamic port forwarding, guest agent, Rosetta, or inherited host environment.
- [ ] Apply exact CPU, memory, disk, and wall-time ceilings.
- [ ] Discover and destroy owned orphan VMs after controller crash or reboot.
- [ ] Prove the lifecycle on the physical acceptance Mac only after fake-executor tests pass.

## Milestone 3 — GitHub just-in-time execution

- [ ] Store a least-privilege GitHub App credential in the Mac Keychain.
- [ ] Integrate a pinned GitHub Runner Scale Set Client behind a narrow local adapter for demand, sessions, acknowledgement, and JIT configuration.
- [ ] Bind the scale-set claim, runner ID/name/labels, actual assigned job, and VM to the durable attempt.
- [ ] Transfer the JIT configuration without argv, logs, public journals, or reusable guest storage.
- [ ] Run the pinned official runner for one job and collect bounded external lifecycle logs.
- [ ] Observe the terminal job, delete stale runner registrations, destroy the VM, and release capacity automatically.
- [ ] Demonstrate the full path against an enrolled test repository without operator commands.

## Milestone 4 — hostile-CI network and container policy

- [ ] Deny inbound access and outbound host, private/LAN, link-local, metadata, controller, and peer-worker destinations outside workload authority.
- [ ] Preserve DNS and ordinary outbound clone/package/build access.
- [ ] Add bounded connection/rate/byte policy and explicit project exceptions.
- [ ] Allow rootless nested containers inside the disposable VM where workflows need container actions or service containers.
- [ ] Verify benign CI and hostile network/resource fixtures.

## Milestone 5 — supervised autoscaling and recovery

- [ ] Add `smolrunner worker serve` as a bounded reconciler supervised by `launchd`.
- [ ] Run the scale-set long-polling listener with durable acknowledgement and session recovery; do not add an inbound webhook for the first path.
- [ ] Scale within host-wide concurrency/resource limits and return to zero running workers when idle.
- [ ] Add backoff, retry budgets, circuit breakers, operator holds, and precise status/remediation.
- [ ] Reconcile controller kill, sleep/wake, reboot, GitHub outage, failed provisioning, stuck job, and failed teardown.
- [ ] Keep secrets and raw repository data out of durable diagnostics.

## Milestone 6 — production acceptance and optimization

- [ ] Repeatedly run a known repository and intentionally hostile fixtures.
- [ ] Measure queue-to-start time, job overhead, peak RAM/disk, teardown time, failure convergence, and idle footprint.
- [ ] Use GitHub Actions cache/artifacts as the initial cache path.
- [ ] Optimize template/image warming only after the one-job disposal boundary is stable.
- [ ] Add local dependency caches only with quotas, namespace isolation, poisoning tests, and no reuse of authoritative compiled outputs.

## Deferred

- Completion of the custom 40-class R01 runtime-readiness graph and host-rootless-Podman hostile-code backend.
- Persistent workers, retained writable workspaces, and shared compiled-output caches.
- Linux fleet stewardship as the primary product path.
- Multi-host selection, cloud providers, Kubernetes, and public multi-tenancy.
- Previews, routing, deployment, and production credentials.
- Automatic self-update, fleet policy, dashboard, and broad automatic repair.

## Non-goals

- Replacing GitHub Actions workflow YAML or the official runner protocol.
- Building a custom VM, container, firewall, package-manager, init, or service-supervision implementation.
- Weakening the isolation boundary to produce a demo.
- Treating known repositories as trusted code.
- Treating successful verification as deployment authority.
- Giving a fleet coordinator, agent, or generated patch unrestricted shell or self-expanded mutation authority.
