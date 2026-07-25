# ADR 0022: Mac availability transition planning

## Status

Accepted for the read-only planning slice.

## Context

The Apple Silicon MacBook guest has two reviewed Lima profiles: an interactive profile with 4 CPUs and 3 GiB of memory, and an away profile with 8 CPUs and 8 GiB of memory. Operators also need a reliable way to disable admission and stop the VM. Ad hoc `limactl` commands cannot safely answer whether a job is active, whether observations are fresh, or whether a profile change is currently allowed.

The eventual public surface should be small:

```text
smolrunner availability active
smolrunner availability away
smolrunner availability off
smolrunner availability auto
```

The first implementation remains pure and read-only. It defines the transition contract consumed later by the CLI, Mac workspace UI, agent API, and host-side executor.

## Decision

Model requested availability separately from effective availability.

Requested modes are:

- `active` — select the interactive profile;
- `away` — select the larger away profile;
- `off` — drain admission and stop the VM;
- `auto` — defer mode selection to a later reviewed policy.

Effective modes are only `active`, `away`, and `off`. `auto` is policy, not observed machine state.

A transition plan is bound to explicit observations of:

- effective mode;
- VM power state;
- runner job activity;
- observation freshness;
- AC or battery power;
- macOS memory pressure;
- operator hold state.

Unknown, stale, conflicting, or held state blocks a transition. Process success or a terminal window being closed is not evidence that the VM or listener is idle.

## Initial profiles

The initial typed profiles match the checked-in Lima examples:

| Mode | CPUs | Memory | Concurrent jobs |
| --- | ---: | ---: | ---: |
| active | 4 | 3 GiB | 1 |
| away | 8 | 8 GiB | 1 |

Concurrency remains one until field measurements justify changing it.

## Transition rules

- Switching between active and away requires an idle-job barrier, runner drain, VM stop, profile application, VM start, and fresh verification.
- Moving to off requires an idle-job barrier, runner drain, VM stop, and fresh verification.
- Moving from off to active or away applies the selected profile, starts the VM, and freshly verifies the result.
- Away mode initially requires AC power and normal memory pressure.
- Active mode may tolerate elevated memory pressure but does not start while pressure is critical or unknown.
- Off remains available under resource pressure, but still requires fresh idle-job evidence.
- Operator hold, stale observation, unknown VM state, unknown job activity, or mode/power inconsistency fails closed.
- Auto mode returns a stable `manual_policy_required` disposition until idle detection, power and memory observation, hysteresis, dwell time, and drain behaviour are implemented and reviewed.

A ready plan is still not mutation authority. The executor must later bind the plan to exact fresh observations, durable journal state, listener identity, and the reviewed Lima configuration before any stop, edit, or start operation.

## Consequences

- Human interfaces, cmux integration, GitHub bridges, and future MCP adapters can consume one stable typed plan.
- UI controls cannot bypass active-job or operator-hold barriers.
- Automatic mode switching remains intentionally unavailable in the first slice.
- The planner has no macOS probing, Lima execution, GitHub access, filesystem writes, or credential access.
- A later implementation must add bounded host observation and durable execution rather than embedding shell recipes into the UI.

## Follow-up

Issue #120 tracks host observation, explicit active/away/off helpers, field measurements, and eventual auto policy. Issue #121 tracks the agent-facing API that may request—but not self-authorise—availability changes.
