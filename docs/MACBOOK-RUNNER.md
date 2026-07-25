# MacBook runner operator guide

This guide defines a conservative Lima boundary for running SmolRunner experiments on an Apple-silicon MacBook Air with 24 GiB of unified memory. It covers VM creation, resource-profile transitions, measurement, sleep handling, and cleanup. It stops before live GitHub runner registration.

SmolRunner remains pre-alpha. The current CLI provides read-only diagnostics and planning. Runner installation, registration, reconciliation, and disposable job execution remain roadmap work.

## Profiles

| Profile | VM memory | VM vCPUs | Initial runner concurrency | Default disposable-job cap | Intended use |
| --- | ---: | ---: | ---: | --- | --- |
| Interactive | 3 GiB | 4 | 1 job | 2 CPUs, 2 GiB RAM | Normal use while the Mac is active |
| Away | 8 GiB | 8 | 1 job | 2 CPUs, 2 GiB RAM | One heavier job after active work finishes |

The repository contains two fresh-instance templates:

- `examples/lima/smolrunner-interactive.yaml`
- `examples/lima/smolrunner-away.yaml`

Both profiles use:

- Lima 2.1.1 or newer;
- Apple Virtualization Framework through `vmType: vz`;
- an ARM64 Ubuntu 24.04 guest;
- an 80 GiB primary disk;
- plain mode;
- an empty mount list;
- disabled SSH-agent and X11 forwarding;
- disabled host SSH public-key loading;
- disabled proxy-environment propagation;
- disabled Lima-managed containerd;
- disabled Rosetta;
- a headless display.

Plain mode is the primary boundary. It disables Lima filesystem sharing, dynamic port forwarding, built-in containerd, the guest agent, Rosetta, and SSH-agent forwarding. The repeated explicit settings make profile review easier and guard against accidental edits.

The VM administrator account is `lima`. The eventual Actions listener and rootless Podman runtime belong under a separate dedicated Linux account created through SmolRunner’s reviewed account path.

## Security boundary

Treat checked-out repository code as untrusted. Preserve these rules throughout every experiment:

- keep the Mac home directory outside the guest;
- keep personal SSH keys, SSH-agent sockets, cloud credentials, browser profiles, password-manager sockets, Git configuration, and GitHub credentials outside the guest;
- keep the official Actions listener in the guest control plane;
- execute repository code only inside a bounded disposable rootless Podman container;
- expose no Podman control socket to repository code;
- deny fork pull-request workloads on the persistent self-hosted runner;
- resolve mutable Git references to immutable commits before execution;
- retain one job at a time until real measurements support a higher limit;
- use exact runner-removal and ownership evidence before deleting any registered runner state.

The profiles contain no provisioning scripts. Package preparation, runner-account creation, official-runner installation, and registration should enter through reviewed SmolRunner paths as they become available.

## Install Lima

Install Lima from Homebrew and record the installed version:

```bash
brew install lima
limactl --version
```

The templates require Lima 2.1.1 or newer. Review profile compatibility before upgrading across a major Lima release.

## Create the interactive VM

Create one instance named `smolrunner`:

```bash
limactl create \
  --name smolrunner \
  examples/lima/smolrunner-interactive.yaml

limactl start smolrunner
```

Inspect the guest identity and basic host capabilities:

```bash
limactl shell smolrunner -- /usr/bin/uname -m
limactl shell smolrunner -- /usr/bin/cat /etc/os-release
limactl shell smolrunner -- /usr/bin/systemctl is-system-running
limactl shell smolrunner -- /usr/bin/findmnt
limactl shell smolrunner -- /usr/bin/env
```

Expected identity:

- architecture: `aarch64`;
- distribution: Ubuntu 24.04;
- init system: systemd;
- no host `/Users/...` filesystem mount;
- no `SSH_AUTH_SOCK`, `GITHUB_TOKEN`, `GH_TOKEN`, AWS, Google Cloud, Azure, or personal proxy credential in the guest environment.

Check the Lima instance directory and cached image footprint on macOS:

```bash
du -sh "${LIMA_HOME:-$HOME/.lima}/smolrunner"
du -sh "$HOME/.cache/lima"
```

The first command reports allocated host storage for the VM directory. The configured 80 GiB virtual disk is sparse and can grow as guest data accumulates.

## Read-only observation helper

Run the included helper before and after a workload:

```bash
bash scripts/macbook-runner-observe.sh smolrunner
```

It reports:

- macOS memory pressure, swap use, VM statistics, and process resource use;
- Lima version, instance state, instance-directory size, and image-cache size;
- guest memory, load, uptime, root-disk use, systemd state, and cgroup memory counters;
- Podman disk use when `/usr/bin/podman` exists.

The helper performs observation only. Redirect its output into timestamped operator notes when collecting measurements.

## Job resource policy

The VM envelope and the disposable-job cap are separate controls.

The initial disposable-job policy is:

```text
concurrency = 1
cpus = 2
memory = 2 GiB
```

The 3 GiB interactive VM leaves about 1 GiB for the guest kernel, systemd, the Actions listener, Podman, and filesystem cache. A workload that regularly consumes the entire 2 GiB job cap belongs in the away profile or a separate heavy-worker VM.

The 8 GiB away profile keeps the same initial job cap. Increase a job’s container CPU or memory cap only after a clean baseline run. Container limits and job admission policy can change before the next job while the VM stays running. VM memory and exposed vCPU changes use a stop/edit/start cycle.

One official Actions runner accepts one job at a time. Running two light jobs requires two independently registered runner listeners with separate dedicated accounts and ownership records. Defer that expansion until the single-runner path is dependable and measured.

## Switch to the away profile

Finish every active job first. Confirm the runner appears idle in GitHub and retain the final job receipt.

Stop the VM, edit the resource envelope, and start it again:

```bash
limactl stop smolrunner
limactl edit smolrunner --cpus 8 --memory 8
limactl start smolrunner
```

Verify the guest after restart:

```bash
limactl shell smolrunner -- /usr/bin/nproc
limactl shell smolrunner -- /usr/bin/free -h
limactl shell smolrunner -- /usr/bin/systemctl is-system-running
```

Return to the interactive profile with the same idle transition:

```bash
limactl stop smolrunner
limactl edit smolrunner --cpus 4 --memory 3
limactl start smolrunner
```

Keep the two YAML files as fresh-instance references. Avoid registering both templates as copies of the same GitHub runner.

## Disk sizing and cleanup

Start with 80 GiB. Record these values periodically:

```bash
df -h
limactl shell smolrunner -- /usr/bin/df -h /
limactl shell smolrunner -- /usr/bin/podman system df
du -sh "${LIMA_HOME:-$HOME/.lima}/smolrunner"
du -sh "$HOME/.cache/lima"
```

Grow the primary disk during an idle restart:

```bash
limactl stop smolrunner
limactl edit smolrunner --disk 120
limactl start smolrunner
```

Lima supports primary-disk growth. Treat shrinking as VM recreation.

Prefer targeted cleanup:

1. inspect Podman containers and images through exact IDs and ownership evidence;
2. remove disposable job containers after their receipts are durable;
3. remove obsolete, proven-owned images by immutable ID;
4. keep runner installation and ownership state until the reviewed runner-removal path succeeds;
5. run `limactl prune` only for unused Lima downloads and cache data;
6. recreate the whole VM when its state is intentionally disposable.

Before deleting an instance, confirm it contains no live or still-registered runner:

```bash
limactl stop smolrunner
limactl delete smolrunner
```

Creation from either profile restores only the VM boundary. It does not restore SmolRunner ownership records or GitHub runner registration.

## Sleep and wake behavior

A Mac sleep can interrupt guest networking, GitHub heartbeats, wall-clock progress, and active job processes.

Use this operating policy:

- keep the Mac awake during an active job with `caffeinate -i`;
- let active jobs finish before closing the lid or changing profiles;
- stop the VM before travel or a long sleep window;
- after an unexpected sleep, treat an in-flight job as interrupted until GitHub and durable receipts agree on its outcome;
- inspect guest time, network reachability, systemd, rootless Podman, and runner status before accepting another job;
- discard the prior disposable job environment and rerun from an immutable commit when the outcome is uncertain.

Wake checks:

```bash
limactl list
limactl shell smolrunner -- /usr/bin/date --iso-8601=seconds
limactl shell smolrunner -- /usr/bin/systemctl is-system-running
limactl shell smolrunner -- /usr/bin/cat /proc/loadavg
limactl shell smolrunner -- /usr/bin/free -h
```

Use graceful `limactl stop` first. Reserve force-stop recovery for an unresponsive VM, then inspect the filesystem and runner state before reuse.

## Measurement procedure

Collect one baseline snapshot before each run and another immediately afterward:

```bash
bash scripts/macbook-runner-observe.sh smolrunner
```

For each workload:

1. record profile, exact repository commit, runner version, guest package versions, and container image digest;
2. record host swap use before the run;
3. record guest memory and disk state before the run;
4. execute the trusted repository job once with cold caches;
5. execute the same immutable commit again with warm caches;
6. record elapsed duration from GitHub and the job receipt;
7. record peak container or cgroup memory from the execution environment;
8. record host swap use after the run;
9. describe typing latency, window switching, video calls, browser responsiveness, and thermal behavior during the run;
10. repeat any surprising result before changing a profile.

Use the same job caps for both profiles during the first comparison.

## Measurement table — awaiting a real MacBook run

Every value below awaits execution on the target 24 GiB Apple-silicon MacBook Air. The profiles and documentation have received static review only.

| Measurement | Interactive: 3 GiB / 4 vCPU | Away: 8 GiB / 8 vCPU | Collection method |
| --- | --- | --- | --- |
| VM idle memory | Awaiting real run | Awaiting real run | Guest `free -h`, cgroup counters, host VM-process RSS |
| Light Rust CI peak memory | Awaiting real run | Awaiting real run | Exact commit, 2-CPU/2-GiB cap, peak cgroup memory |
| Node or Vite build peak memory | Awaiting real run | Awaiting real run | Exact commit, 2-CPU/2-GiB cap, peak cgroup memory |
| Browser-test peak memory | Awaiting real run | Awaiting real run | Exact commit, bounded browser container, peak cgroup memory |
| Cold run duration | Awaiting real run | Awaiting real run | GitHub job duration and durable receipt |
| Warm run duration | Awaiting real run | Awaiting real run | Same immutable commit and image digest |
| Host swap delta | Awaiting real run | Awaiting real run | `sysctl vm.swapusage` before and after |
| Host responsiveness | Awaiting real run | Awaiting real run | Operator notes during the workload |

## Criteria for a separate heavy-worker VM

Move a workload to a separate temporary heavy-worker VM when any of these provisional guardrails repeats across two comparable runs:

- the job reaches its 2 GiB cap and requires a larger limit;
- the 8 GiB guest leaves less than 1 GiB available for the listener, Podman, and the OS;
- host swap grows by more than 1 GiB during one job;
- macOS memory pressure enters a warning or critical state;
- the Mac becomes visibly sluggish during normal interactive work;
- a warm run takes more than twice its established baseline;
- browser tests require several concurrent browser processes or a larger shared-memory allocation;
- the workload requires x86_64 system emulation, nested virtualization, or another materially different trust boundary.

A temporary heavy worker should receive its own VM, runner account, runner registration, labels, ownership records, and cleanup receipt. Stop it after the job so its reserved RAM returns to macOS.

## Sources

- [SmolRunner threat model](THREAT_MODEL.md)
- [Lima VZ configuration](https://lima-vm.io/docs/config/vmtype/vz/)
- [Lima plain mode](https://lima-vm.io/docs/config/plain/)
- [Lima edit command](https://lima-vm.io/docs/reference/limactl_edit/)
- [Lima disk management](https://lima-vm.io/docs/config/disk/)
