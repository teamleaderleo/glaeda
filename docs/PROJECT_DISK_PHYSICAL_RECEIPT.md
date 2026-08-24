# Project-disk physical receipt

This runbook is the read-only acceptance slice required before issue #565 P2 can become production authority.

The receipt answers an evidence question only: what exact host-side standalone-disk directory and direct entries were present while one exact resident Lima VM exposed one exact Linux filesystem device at the declared project mount? The declared SmolRunner project-disk, attachment, and resident-sandbox generations are labels carried into the receipt for later correlation review. They gain no authority from the receipt by themselves.

## Boundary

The collector performs these observations:

- descriptor-bound Lima host identity using the existing VZ identity adapter;
- descriptor-relative traversal of an operator-supplied exact standalone-disk directory;
- direct-entry type, device/inode, ownership, mode, link count, logical bytes, allocated bytes, and timestamps;
- bounded bytes for direct regular files up to 64 KiB and the raw target bytes of direct symlinks;
- `limactl --version`;
- `limactl disk list --json` as opaque JSON-stream evidence;
- the repo's existing exact `limactl --tty=false list --format=json --all-fields <instance>` observation form;
- guest `/proc/self/mountinfo` and `lsblk --json --bytes` without filesystem UUID or superblock probing.

The collector performs zero project-disk mutation. Attach, detach, stale-lock unlock, format, resize, delete, guest mount mutation, and OverlayFS mount mutation remain outside this slice. Project filesystem contents and project filesystem superblock fields are excluded from cleanup ownership evidence.

The collector deliberately has no `_disks/<name>` derivation. `--disk-directory` is required and must be the exact directory observed on the pinned operator Mac. Disk names stay locators. Direct entry names stay opaque in the parser; the first real receipt is what tells review which current entry is the VZ backing file and how the current lock is represented.

## Why this is manual operator-Mac acceptance

The owned hosted-macOS probe for #565 found `limactl` absent, so hosted Actions cannot produce the required current standalone-disk receipt from the already-owned Lima installation. The repository also has no accepted self-hosted operator-Mac runner label for this experiment. Run this command directly from the operator Mac's clean checkout, following the same private-receipt discipline used by the personal-worker physical acceptance runbook.

## Before capture

Use the exact commit under review and the already-installed pinned Lima environment. Keep ordinary project work idle for the short observation window so the receipt represents one coherent attachment.

Record these alongside the private receipt:

```sh
git status --porcelain=v1 --untracked-files=all
git rev-parse HEAD
sw_vers
uname -a
"$(command -v limactl)" --version
```

Set the inputs from the current controlled disk/attachment experiment. In particular, set `PROJECT_DISK_DIRECTORY` from direct observation of the installed Lima state. If the exact directory itself is still unknown, capture `limactl disk list --json` separately and stop there; do not manufacture a path from remembered Lima layout.

The generation values must be the exact currently declared #565 P1 values for this controlled disk attachment:

```sh
export LIMA_HOME='... exact current Lima home ...'
export PROJECT_DISK_DIRECTORY='... exact observed standalone-disk directory ...'
export PROJECT_DISK_NAME='... current Lima disk locator ...'
export LIMA_INSTANCE='... exact resident sandbox instance ...'
export GUEST_PROJECT_MOUNT='... exact mounted project path in the guest ...'
export GUEST_CACHE_PATH='... exact guest cache path used by the existing Lima host-identity request ...'
export PROJECT_DISK_ID='... exact SmolRunner project-disk ID ...'
export PROJECT_DISK_GENERATION='...'
export ATTACHMENT_GENERATION='...'
export RESIDENT_SANDBOX_ID='... exact SmolRunner resident-sandbox ID ...'
export RESIDENT_SANDBOX_GENERATION='...'
export RECEIPT="$HOME/project-disk-physical-receipt.private.json"
```

Require a fresh output pathname. The example uses `create_new` with mode `0600` and refuses to overwrite an existing receipt.

## Capture

```sh
cargo run --locked --bin project_disk_physical_receipt -- \
  --repo-commit "$(git rev-parse HEAD)" \
  --lima-home "$LIMA_HOME" \
  --disk-directory "$PROJECT_DISK_DIRECTORY" \
  --disk-name "$PROJECT_DISK_NAME" \
  --resident-sandbox-instance "$LIMA_INSTANCE" \
  --guest-project-mount "$GUEST_PROJECT_MOUNT" \
  --guest-cache-path "$GUEST_CACHE_PATH" \
  --limactl "$(command -v limactl)" \
  --project-disk-id "$PROJECT_DISK_ID" \
  --project-disk-generation "$PROJECT_DISK_GENERATION" \
  --attachment-generation "$ATTACHMENT_GENERATION" \
  --resident-sandbox-id "$RESIDENT_SANDBOX_ID" \
  --resident-sandbox-generation "$RESIDENT_SANDBOX_GENERATION" \
  --output "$RECEIPT"
```

The command holds the supplied directory and openable direct entries across the Lima/guest observations, brackets the transaction with the repo’s descriptor-bound Lima VZ host-identity observer, and then rebinds the exact Lima home and disk directory. Entry replacement changes device/inode identity and fails the receipt. Large regular files are never read; their logical and allocated byte counts are recorded from host metadata. Small regular files and symlink targets are captured only because the unknown current lock representation may live there. Treat the complete receipt as private operator evidence.

## Review the first real receipt

The first accepted receipt must establish the current installed schema before #565 P2 code assigns roles to direct entries. Review these correlations together:

1. Exact repo commit, macOS version, Lima version, existing Lima host-identity digest, and resident sandbox instance.
2. Exact supplied disk directory and its complete bounded direct-entry set.
3. Which observed regular entry is the actual standalone-disk backing file, with logical and allocated bytes from both snapshots.
4. Exact current `in_use_by` representation from the observed entry type/bytes or symlink target, plus its relationship to the resident sandbox observation.
5. `limactl disk list --json` and exact instance JSON as external Lima observations only.
6. Guest mountinfo's one exact `major:minor`, filesystem type, and source at `GUEST_PROJECT_MOUNT`, with raw `lsblk` JSON retained for current block-device correlation.
7. The declared SmolRunner project-disk generation, attachment generation, resident-sandbox ID, and resident-sandbox generation that the operator intended to observe.

A disk name match or Lima lock match never becomes SmolRunner ownership by itself. A later production observer must bind host-controlled physical identity to the durable SmolRunner generations and detect same-name replacement through descriptor rebind evidence.

## Gate to #565 P2 and #589

After one real receipt establishes the exact installed standalone-disk schema, implement #565 P2 against that evidence only:

- encode the proven backing-file and lock representation descriptor-relatively;
- retain exact file/directory identity across observation and rebind it before returning authority;
- classify Lima JSON and lock state as observations;
- keep stale-lock unlock, format, resize, and delete as distinct explicit mutation classes;
- keep project filesystem content and superblock data outside cleanup ownership authority;
- bind the resulting physical observation to the exact durable project-disk and attachment/resident-sandbox generations.

That accepted P2 observer becomes the sole production constructor for #589's project-filesystem correlation proof. Test-only constructors can continue to exercise pure logic, while production trusted OverlayFS mount mutation remains disabled until the real physical correlation proof is available and separately wired through the reviewed authority path.
