# macOS resource observation

`src/macos_resource_observation.rs` defines the read-only host-resource evidence boundary used by later Mac availability and Lima-worker status adapters.

The observer runs four reviewed absolute macOS programs with fixed argument shapes and an empty environment:

- `/usr/sbin/sysctl -n kern.memorystatus_vm_pressure_level` for the discrete system pressure level;
- `/usr/sbin/sysctl -n vm.swapusage` for total, used, free, and optional encryption state;
- `/usr/bin/pmset -g batt` for AC or battery source, charge percentage, and charge state;
- `/bin/ps -axo pid=,ppid=,%cpu=,rss=,etime=,comm=` for bounded Lima-related process resources.

No shell, `awk`, caller-selected command, environment value, Lima profile decision, start, stop, edit, queue query, job control, timer, polling loop, credential access, persistence, or mutation is part of this boundary.

## Process attribution

A process is public Lima evidence only when it is an exact `limactl`, `lima`, or `lima-*` root or a descendant of one of those roots in the same bounded process snapshot. A process that merely resembles a VM helper, such as an unrelated QEMU process, is not attributed to Lima.

Public process output contains only:

- PID and parent PID;
- a bounded role (`controller`, `host_agent`, `virtual_machine`, `network`, `file_sharing`, or `auxiliary`);
- CPU basis points;
- resident bytes;
- elapsed seconds.

Executable paths, command names, arguments, raw process rows, and all other command output remain private and are redacted from `Debug` and JSON output. Consumers receive the versioned report, not the retained command receipts. Debug output may expose only the number of captured sources beside a fixed redaction marker.

## Fail-closed evidence

Command, shape, size, parser, duplicate-PID, or consistency failures become fixed public problem kinds. They are never converted to zero usage, no swap, AC power, normal pressure, or no Lima processes.

Only the canonical pressure values `1` (normal), `2` (elevated), and `4` (critical) are accepted; every other value remains `unknown`. Swap totals must reconcile within the fixed rounding tolerance. Observation time and freshness are supplied explicitly by the caller, and stale evidence remains visible.

This module produces observations only. The existing `mac_availability` policy remains the authority for blockers and transition planning, while exact Lima instance configuration and guest observation remain a separate issue #171 adapter.
