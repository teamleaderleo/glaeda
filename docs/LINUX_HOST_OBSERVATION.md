# Linux host observation

`glaeda-host-observe` returns one bounded read-only snapshot for recurring operator-machine
handoffs:

```bash
glaeda-host-observe --output json
glaeda-host-observe --output human --port 3000 --port 8080
```

With no `--port`, the command watches a fixed small set of common development ports. Supplying one
or more ports replaces that default. At most 32 positive TCP ports are accepted.

## Report

One typed human/JSON report contains:

- logical CPU count plus 1/5/15-minute load and runnable/total entity counts from `/proc`;
- total/available memory and total/used swap;
- CPU, memory, and I/O PSI `some` averages and cumulative microseconds;
- scheduler autogroup support/state plus stable sched_ext support/state, enable sequence, and active ops
  name when enabled;
- failed system and current-user systemd unit counts;
- only the requested/default port numbers and whether each is listening.

The command reads fixed kernel interfaces directly and invokes only fixed `/usr/bin/systemctl`
system and user failed-unit observations. Child environments are rebuilt from an allowlist; the
user-manager paths are derived from the effective UID. Each child has a two-second deadline.
Unavailable systemd managers are represented as `knowledge=unavailable` while valid kernel facts
remain useful. Missing, oversized, or malformed required kernel data fails the report.

## Privacy and authority

The report intentionally excludes paths, addresses, unit names, PIDs, process command lines,
environment values, repository content, and arbitrary logs. Its scope is
`current_execution_context`: it describes the process's visible procfs/network namespace and
reachable systemd managers. It does not prove physical-host identity or that the caller is outside
a container or nested namespace.

`authority=observation_only` is literal. The command does not admit work, establish ownership,
adopt or terminate processes, mutate services or listeners, select resources, clean state, or
grant cache/result authority. Consumers must treat a dynamic snapshot as evidence observed over a
short interval, not as an atomic machine transaction.

The sched_ext fields are observation, not a scheduler selector. The observer rereads state and the
monotonic enable sequence around the active-ops name and refuses a transition instead of emitting a
mixed snapshot. An absent fixed sched_ext sysfs directory is reported as unsupported; malformed or
partially available scheduler data fails the report.

## Performance evidence

The exact big-red controls and measured matrix are recorded in
[`DEVELOPER_LOOP_BENCHMARK.md`](DEVELOPER_LOOP_BENCHMARK.md#bounded-linux-machine-observation--2026-08-30).
The compiled command materially reduced complete handoff-observation latency and process count
against both the ordinary shell composition and a single-process Python control.
