# Runner account hygiene

`glaeda-runner-hygiene` installs the automatic disposable-cache sweep for a
named secondary macOS account. It is an operator command and plans by default:

```sh
/path/to/reviewed-release/scripts/glaeda-runner-hygiene --user cmux
sudo /path/to/reviewed-release/scripts/glaeda-runner-hygiene --user cmux --apply
sudo /path/to/reviewed-release/scripts/glaeda-runner-hygiene --user cmux --uninstall
sudo /path/to/reviewed-release/scripts/glaeda-runner-hygiene --user cmux --uninstall --apply
```

Use the exact merged release checkout (or pass its `--source` path). The
user-writable convenience copy under `~/.local/bin` is not an authority for a
root install.

The account must be a named non-system account with a real, user-owned home
directory. `--apply` requires root. It copies the exact `glaeda-disk` source to
`/Library/Application Support/glaeda/runner-hygiene/<uid>/glaeda-disk`, records
its SHA-256 and the account home/filesystem device in a root-owned manifest, and
installs the matching root-owned plist at
`/Library/LaunchDaemons/com.teamleaderleo.glaeda.runner-hygiene.<uid>.plist`.
The copy is written and renamed atomically, mode `0555`, and marked immutable
when macOS supports `chflags`.

The LaunchDaemon has `UserName` set to the selected account and sets `HOME` to
that account's passwd home. It never runs the cleanup as root. Every 15 minutes
it executes the pinned copy with Python isolated mode and the bounded runner
cache profile:

```text
python3 -I <pinned-glaeda-disk> --runner-caches --pressure --idle --apply --top 0
```

The allowlist is five families: Xcode `DerivedData`, cmux job caches,
GhosttyKit caches, CUA recordings, and CUA frame caches. Each item must be
idle for at least 168 hours. A pressure wake happens every 15 minutes; when
the disk is healthy, the idle sweep is throttled to once per hour. Git index
refreshes do not count as use, while process, open-file, command-line, socket,
and git-loss checks still veto a deletion.

The daemon's stdout and stderr go to `/dev/null`; the disk tool's own receipt
is the bounded deletion record. Existing plist/install paths are accepted only
when their management marker and account binding match. Unrelated files,
symlinks, and edited plists are refused. Planning performs no writes and does
not invoke `launchctl`; apply uses ten-second bounded `launchctl` calls.

This job only covers the selected account's rebuildable runner caches. It does
not remove wallpapers, credentials, SSH material, runner registration, shared
CI stores, or another account's data.

The normal mini setup and OTA release install the operator command beside the
other Glaeda tools, but they do not guess which old accounts are disposable.
Run the `--apply` install once for each account that is actually enrolled as a
runner. After that, no hand step keeps it current: the root
`glaeda-daemon-heal` LaunchDaemon, which already follows the stable release
through the verified channel, keeps that release's `glaeda-disk` and
`glaeda-runner-hygiene` in its own root-owned snapshot. On each pass, for every
account that already has a job here, it runs that release's
`glaeda-runner-hygiene --user NAME --check` and, when the snapshot or plist is
not what its `--apply` leaves, that `--apply`. An unchanged, current job is
checked again every 6 hours; a failed reinstall is retried after 15 minutes.
Its report (`runnerHygiene`) feeds `glaeda-mini-health`'s
`disk_root_copy_stale:<uid>`. OTA (`glaeda-update`, the runner user) still only
updates the user-writable helper, and nothing adopts an unenrolled account.

```sh
glaeda-runner-hygiene --user cmux --check    # read-only: current or stale, and why
```

A plist that an older release wrote (its bytes are the ones the manifest
recorded) is replaced; a plist edited by hand is still refused.

Before removing a runner account or running `glaeda-mini-setup --uninstall`,
uninstall its root job first with `--uninstall --apply`; the normal setup
uninstall does not own or remove this separate LaunchDaemon.

## Root disk ledger

The runner-account passes cannot see root-only directories or other processes'
open-deleted files, and `du` counts every APFS clone in full, so part of each
host's used space had no name. `glaeda-disk --root-ledger` accounts for it as
root, once a day, and only reports: it deletes nothing but its own expired
snapshots. The same installer puts it in place:

```sh
/path/to/reviewed-release/scripts/glaeda-runner-hygiene --root-ledger           # plan, no writes
sudo /path/to/reviewed-release/scripts/glaeda-runner-hygiene --root-ledger --apply
glaeda-runner-hygiene --root-ledger --check                                      # read-only
sudo /path/to/reviewed-release/scripts/glaeda-runner-hygiene --root-ledger --uninstall --apply
```

On a fleet mini the operator form is `fleet-sudo HOST <release>/scripts/glaeda-runner-hygiene --root-ledger --apply`.
`--apply` pins `glaeda-disk` at
`/Library/Application Support/glaeda/root-ledger/bin/glaeda-disk`, compiles
`scripts/glaeda-cdu.c` beside it into `glaeda-cdu` with `/usr/bin/cc` (without
a compiler the ledger walks with `du` and its snapshot says `"tool": "du"`), and
installs `/Library/LaunchDaemons/com.teamleaderleo.glaeda.root-ledger.plist`:
root (no `UserName`), an hourly wake, `Nice` 10, `LowPriorityIO`, background
process type. The run itself also throttles its disk I/O. `glaeda-daemon-heal`
does not yet keep this install on the stable release; rerun `--apply` from a new
release to update it.

Each wake runs the ledger only when the last snapshot is 20 hours old and
`host_busy` finds no CI job, build, dev-build worker job, reservation or held
host lock; otherwise it waits for the next wake and records why in
`latest.json`'s `last_skip`. A pass:

- walks `/System/Volumes/Data` on one volume with `glaeda-cdu`, bounded to an
  hour: a file that may share blocks (`EF_MAY_SHARE_BLOCKS`) has its physical
  extents (`F_LOG2PHYS_EXT`) marked in a block bitmap, so every block counts
  once, to the first path that reaches it; hard links count once. It does
  not enter the folders macOS guards with TCC under any `Library` (News, Mail,
  Messages, Safari, Cookies, `Containers/com.apple.*`, Apple's Group
  Containers, `Application Support/AddressBook`, `CallHistoryDB`,
  `com.apple.TCC` and the rest of `TCC_SKIP` in `glaeda-cdu.c`): as root a read
  there can block in the kernel. A watchdog bounds every directory read to 60 s;
  a read it cannot interrupt ends the walk with exit 3, and the ledger walks
  again without that path (up to 3 times) and remembers it for 30 days
  (`state/learned-skips.json`). Every skipped or stalled path is in the
  snapshot's `walk.skipped`;
- lists open-deleted files with `lsof +L1`, each device and inode once, by
  process;
- reads purgeable space (important-usage capacity over free) and the inode
  count (APFS metadata, about 2 KiB per inode);
- writes `state/snapshots/ledger-<epoch>.json` (paths of 16 MiB or more, to
  depth 3) and keeps 14 days of them, with per-path and total deltas against
  the previous snapshot;
- writes `state/latest.json` (root-owned, world-readable, bounded).

The runner account's own `glaeda-disk` pass folds `latest.json` into
`~/.local/state/glaeda/disk/summary.json` as `root_ledger`. When the walk plus
open-deleted bytes cover less than 97% of `df` used, it adds the finding
`disk_ledger_gap`. Every byte outside the walk gets one category:
`open-deleted:<process>`, `purgeable`, `apfs-metadata`, `churn` (df used moving
during the walk, and any remainder no directory shows), then what the walk could
not see: `sip`, `tcc-skipped`, `sip+tcc` or `walk-incomplete`. The finding names
the largest category that is not open-deleted and lists the top three. A ledger with no snapshot for 3 days adds `disk_ledger_stale`.
`glaeda-mini-health` reports both like any other glaeda-disk finding.
