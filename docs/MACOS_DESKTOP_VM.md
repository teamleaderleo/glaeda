# macOS desktop-test VM on a fleet mini

This is the checked-in control contract for the optional desktop-test VM. It is
one long-lived macOS guest per eligible M4 Pro mini, started only when a GUI
job is waiting and the host has headroom. It does not build products and it
does not contain a warm checkout. E2E and app-host jobs receive the product
artifact from CI in the same way as the existing reuse path.

The pilot backend is [Lume](https://github.com/trycua/cua), pinned by the fleet
image receipt. Lume uses Apple's Virtualization.framework, supports unattended
macOS guests with a native display, and is MIT licensed. Tart is not a runtime
dependency. The host adapter is [`scripts/glaeda-macos-vm`](../scripts/glaeda-macos-vm):
`plan` is read-only, while `apply` performs one fresh-observation-bound start or
stop and verifies the resulting state. A missing tool, unknown VM identity,
unknown job activity, stale inventory, or failed post-action observation blocks
the action.

The base image is macOS 26.5 or newer with the pinned Xcode command-line tools,
the Actions runner, the Glaeda hook, the UI-test account, and the reviewed
Accessibility, Screen Recording, and Automation grants. The grants are made in
the image's logged-in Aqua session, then verified by a screenshot and a
reversible input probe. The VM is configured for 8 vCPUs, 16 GiB RAM, and an
80 GiB sparse disk. Its break-glass recovery copy is a stopped Lume clone; it
is never reset or cloned per job.

The guest runner uses the same GUI label as the mini's host GUI runner, so the
CI route remains a GUI-only route. Its hook uses the shared capacity ledger and
the `gui-vm` token. The host GUI runner keeps `gui`. The two independent token
files represent the two desktop sessions, while the common unit and root locks
still prevent the VM from competing with builds. The capacity directory is the
only host path shared into the guest; no source tree, credential directory,
runner work tree, or product cache is shared.

Elastic policy uses a one-second `top` sample and the macOS VM pressure level.
At 85% CPU busy or pressure level 2/4, a running VM stops admitting new jobs.
If its current job is idle, the controller gracefully stops the VM to release
CPU and memory. If a job is active, the controller records a blocked plan and
waits; it never suspends or kills a job. When CPU is at most 70%, pressure is
normal, free space is at least 120 GiB, and a GUI job is waiting, the controller
starts the stopped VM with a native display. The hysteresis avoids start/stop
flapping. A queue-depth signal is supplied by the fleet scheduler; an offline
runner is not treated as proof that a job is waiting.

## Pilot gate

The disk gate is evaluated before image creation and before every start. The
pilot begins on cmux10s, then cmux11s and cmux12s only after each has at least
120 GiB free. For each mini, record the exact Glaeda and Lume image receipts,
guest macOS/Xcode versions, host pressure samples, job admission decisions,
pass rate, p50/p95 duration, and host CPU and memory pressure. Compare the
same E2E/app-host legs on the host GUI runner and the VM runner for several
hours. A timing-sensitive Metal, lag, or recorded-tour case remains host-only
until it passes the same acceptance probe in the guest.

Rollback is `glaeda-macos-vm plan` with no queue signal followed by an idle
`apply` stop. Runner labels are removed only after the guest runner is drained.
Deleting the VM is a separate, explicit recovery action and is not part of
elastic reconciliation.
