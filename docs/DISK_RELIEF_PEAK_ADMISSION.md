# Disk-relief peak admission

Disk relief must not start an operation whose temporary working set can consume
the host's remaining space. Git garbage collection and repack write replacement
packs before removing the old packs, so Glaeda estimates their peak as twice the
current object-store bytes and admits the child only when:

```text
free bytes >= projected peak bytes + free-space floor
```

The admission check runs after ownership and process evidence is refreshed and
immediately before the child starts. A pressure pass performs its inexpensive,
owner-verified worktree and cache deletions first; a failed peak check records
the free space, projected peak, floor, and reason in the receipt for a later
pass. The floor is the configured runner admission floor when one exists, or
the policy's 4 GiB minimum on other hosts.

This is a class rule for every Git rewrite reached through `_gc_clones`,
including home-clone GC, runner-checkout repack, and shared-mirror compact. A
new disk-relief action with a temporary working set must use the same admission
helper before starting its child.

Detector: `scripts/test-glaeda-disk.py`,
`test_disk_relief_repack_waits_for_its_peak_and_free_space_floor` verifies the
guard and receipt fields; `test_home_clone_gc_runs_on_pressure_and_sweeps_of_fleet_minis_only`
verifies cheap relief runs before clone GC.
