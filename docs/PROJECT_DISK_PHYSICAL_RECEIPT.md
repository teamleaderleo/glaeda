# Project-disk physical observation receipt

Status: read-only acceptance prerequisite for #565 P2  
Receipt schema: `smolrunner-project-disk-physical-observation` v1

## Purpose

This runbook captures the physical evidence needed before #565 P2 can mint project-disk ownership
proof. The target claim is:

> This exact Linux filesystem device belongs to this exact SmolRunner project-disk generation
> attached to this exact resident sandbox generation.

The first slice grants zero attach, detach, stale-lock unlock, format, resize, delete, cleanup,
OverlayFS mount, or SmolRunner ownership authority. It records observations for review.

The repository carries no baked-in standalone-disk backing filename, `_disks/<name>` derivation,
lock filename, lock meaning, or disk-inventory JSON command. The operator supplies the exact disk
directory and pre-captured JSON from the installed Lima version.

## Existing discipline reused

`tests/project_disk_physical_receipt_host_identity.rs` calls the existing
`LimaHostIdentityAdapter`. That adapter already supplies the VZ/aarch64 restriction,
descriptor-relative private Lima-home traversal, no-follow opens, held-entry rebinding checks, and
bounded VZ identifier/root-disk identity. The physical harness emits only an opaque host identity
digest plus the request digest.

The resident-instance observation reuses the fixed command form already present in
`LimaObservationAdapter`:

```text
limactl --tty=false list --format=json --all-fields INSTANCE
```

Disk names and Lima lock observations remain correlation inputs. SmolRunner ownership comes from the
later #565 P2 binding to durable project-disk and resident-sandbox generations.

## Private evidence boundary

Keep the complete physical receipt private. It contains the exact standalone-disk directory, direct
entry names, symlink targets, exact guest project mountpoint, and selected Lima JSON.

The collector reads no project filesystem contents or project filesystem superblock. First-pass host
inspection reads metadata for every direct standalone-disk-directory entry and symlink targets.
Regular-file bytes are read only when an exact direct entry is supplied with `--read-small-entry`,
with a 4096-byte limit. This second-pass option exists for a small Lima metadata entry after the
physical first pass reveals its exact representation. Never supply the backing entry to
`--read-small-entry`.

## Preconditions

Use a clean checkout at the exact commit under review. Select these values from current accepted
state and direct physical observation:

```bash
export LIMA_HOME='... exact private Lima home ...'
export INSTANCE='... exact resident Lima instance ...'
export GUEST_CACHE_PATH='... exact reviewed guest cache path ...'
export PROJECT_MOUNT='... exact project filesystem mountpoint in the guest ...'
export DISK_DIRECTORY='... exact absolute standalone-disk directory observed on this Mac ...'

export PROJECT_IDENTITY='... exact canonical project identity ...'
export PROJECT_DISK_ID='... exact P1 project-disk ID ...'
export PROJECT_DISK_GENERATION='... exact P1 disk generation ...'
export PROJECT_DISK_REVISION='... exact P1 lease revision ...'
export ATTACHMENT_GENERATION='... exact P1 attachment generation ...'
export RESIDENT_SANDBOX_ID='... exact P1 resident-sandbox ID ...'
export RESIDENT_SANDBOX_GENERATION='... exact P1 resident-sandbox generation ...'
```

The P1 values above are declared correlation labels. The receipt explicitly records physical
ownership as unresolved until #565 P2 validates the accepted physical schema.

Create a private scratch location:

```bash
umask 077
PRIVATE_RECEIPT_DIR="$(mktemp -d)"
LIMACTL="$(command -v limactl)"
test -n "$LIMACTL"
git rev-parse HEAD > "$PRIVATE_RECEIPT_DIR/smolrunner-commit.txt"
"$LIMACTL" --version > "$PRIVATE_RECEIPT_DIR/lima-version.txt"
```

## 1. Capture installed Lima disk JSON without encoding a CLI guess

Record the installed command surface first:

```bash
"$LIMACTL" --help > "$PRIVATE_RECEIPT_DIR/limactl-help.txt"
"$LIMACTL" disk --help > "$PRIVATE_RECEIPT_DIR/limactl-disk-help.txt"
```

Use those exact help files to choose the installed version's read-only JSON disk-inventory command.
Run that command manually and save the selected disk evidence as:

```text
$PRIVATE_RECEIPT_DIR/lima-disk.json
```

Preserve the exact command in a private adjacent note. The repository parser intentionally accepts
JSON evidence as an opaque object/array and carries no disk-list flag or field-name assumption.

Use the same direct observation to determine `DISK_DIRECTORY`. Supply the exact absolute directory
you observed. The collector never derives it from a disk name.

## 2. Capture the exact resident-instance JSON

```bash
HOME=/var/empty \
LIMA_HOME="$LIMA_HOME" \
LANG=C \
LC_ALL=C \
PATH=/usr/bin:/bin:/usr/sbin:/sbin \
"$LIMACTL" --tty=false list --format=json --all-fields "$INSTANCE" \
  > "$PRIVATE_RECEIPT_DIR/resident-instance.json"
```

## 3. Capture the exact guest filesystem-device evidence

Use the GNU `stat` form already used by the repository's Lima observation code:

```bash
HOME=/var/empty \
LIMA_HOME="$LIMA_HOME" \
LANG=C \
LC_ALL=C \
PATH=/usr/bin:/bin:/usr/sbin:/sbin \
"$LIMACTL" --tty=false shell "$INSTANCE" -- \
  /usr/bin/stat -Lc '%d:%i' -- "$PROJECT_MOUNT" \
  > "$PRIVATE_RECEIPT_DIR/guest-project-stat.txt"
```

Capture the kernel mount table independently:

```bash
HOME=/var/empty \
LIMA_HOME="$LIMA_HOME" \
LANG=C \
LC_ALL=C \
PATH=/usr/bin:/bin:/usr/sbin:/sbin \
"$LIMACTL" --tty=false shell "$INSTANCE" -- \
  /usr/bin/cat /proc/self/mountinfo \
  > "$PRIVATE_RECEIPT_DIR/guest-mountinfo.txt"
```

The parser requires exactly one decoded mountinfo row for `PROJECT_MOUNT` and records mount ID,
parent ID, kernel major/minor device, filesystem type, source, and options. These values remain
observation tokens.

## 4. Capture the descriptor-bound Lima host identity

```bash
SMOLRUNNER_TEST_LIMA_HOME="$LIMA_HOME" \
SMOLRUNNER_TEST_LIMA_INSTANCE="$INSTANCE" \
SMOLRUNNER_TEST_GUEST_CACHE_PATH="$GUEST_CACHE_PATH" \
cargo test --locked --test project_disk_physical_receipt_host_identity \
  -- --ignored --nocapture \
  | sed -n 's/^SMOLRUNNER_PROJECT_DISK_HOST_IDENTITY_V1 //p' \
  > "$PRIVATE_RECEIPT_DIR/lima-host-identity.json"

test "$(wc -l < "$PRIVATE_RECEIPT_DIR/lima-host-identity.json" | tr -d ' ')" = 1
```

This step performs the repository's existing descriptor-bound host identity observation. The emitted
JSON contains schema/type, exact resident instance locator, opaque host identity digest, and opaque
observation-request digest.

## 5. First-pass standalone-disk directory capture

Choose a fresh output path; the collector creates it mode `0600` and refuses overwrite.

```bash
python3 scripts/project_disk_physical_receipt.py capture \
  --host-identity-json "$PRIVATE_RECEIPT_DIR/lima-host-identity.json" \
  --disk-directory "$DISK_DIRECTORY" \
  --lima-disk-json "$PRIVATE_RECEIPT_DIR/lima-disk.json" \
  --resident-instance-json "$PRIVATE_RECEIPT_DIR/resident-instance.json" \
  --resident-sandbox-instance "$INSTANCE" \
  --guest-project-stat "$PRIVATE_RECEIPT_DIR/guest-project-stat.txt" \
  --guest-mountinfo "$PRIVATE_RECEIPT_DIR/guest-mountinfo.txt" \
  --guest-project-mountpoint "$PROJECT_MOUNT" \
  --project-identity "$PROJECT_IDENTITY" \
  --project-disk-id "$PROJECT_DISK_ID" \
  --project-disk-generation "$PROJECT_DISK_GENERATION" \
  --project-disk-revision "$PROJECT_DISK_REVISION" \
  --attachment-generation "$ATTACHMENT_GENERATION" \
  --resident-sandbox-id "$RESIDENT_SANDBOX_ID" \
  --resident-sandbox-generation "$RESIDENT_SANDBOX_GENERATION" \
  --output "$PRIVATE_RECEIPT_DIR/project-disk-first-pass.json"

python3 scripts/project_disk_physical_receipt.py validate \
  "$PRIVATE_RECEIPT_DIR/project-disk-first-pass.json" \
  > /dev/null
```

The first pass records every direct entry as opaque evidence:

- exact entry-name bytes and UTF-8 view when available;
- exact file kind;
- device/inode, owner, mode, link count, logical bytes, allocated bytes (`st_blocks * 512`),
  mtime/ctime, plus platform birthtime/generation fields when exposed;
- exact symlink target bytes;
- zero inferred backing or lock role.

The collector holds the directory descriptor while entries are inspected, rechecks every direct
entry after inspection, then reopens and rebinds the exact directory before publication. A changed
or same-name-replaced entry/directory refuses the receipt.

## 6. Capture a small regular lock representation when the first pass requires it

A symlink candidate already includes its exact target. When the physical first pass instead shows a
small regular metadata entry whose bytes are required to understand the observed lock, rerun with:

```text
--read-small-entry EXACT_OBSERVED_METADATA_ENTRY
```

The entry must be a captured direct regular file no larger than 4096 bytes. The collector holds the
file descriptor and verifies exact metadata before and after the bounded read.

## 7. Add explicit physical role labels after inspection

After the first pass establishes the installed layout, rerun with both exact direct names selected
from that receipt:

```text
--observed-backing-entry EXACT_OBSERVED_BACKING_ENTRY
--observed-lock-entry EXACT_OBSERVED_LOCK_ENTRY
```

These labels carry `source: explicit_operator_observation`. The parser requires both names to exist
in the captured direct-entry set and requires distinct entries. It applies zero filename, file-kind,
or lock-semantics convention.

## Evidence required before #565 P2 implementation

One reviewed physical receipt must establish this coherent observation set:

1. exact descriptor-bound resident VZ host identity and request identity;
2. exact standalone-disk directory and complete bounded direct-entry set;
3. exact observed backing entry with logical and allocated bytes;
4. exact observed lock entry, kind, and target/content representation;
5. exact installed Lima disk JSON showing the selected disk/attachment observation;
6. exact resident-instance JSON for the selected resident sandbox;
7. exact guest project mountpoint `stat` device token plus mountinfo major/minor/filesystem row;
8. exact P1 project-disk generation/revision and attachment/resident-sandbox generations carried as
   declared correlation labels;
9. same-name replacement detection through entry/directory descriptor identity changes.

The physical receipt remains research evidence. #565 P2 must encode the accepted installed schema
descriptor-relatively and bind host-controlled physical identity to the exact durable SmolRunner
project-disk and attachment/resident-sandbox generations. Lima names and lock observations remain
locators/correlation evidence.

## Stop rules

Keep #565 P2 blocked when any expected entry role remains ambiguous, any observed object drifts,
disk/instance evidence disagrees, the guest mountpoint has zero or multiple exact mountinfo rows, or
same-name replacement cannot be distinguished.

Keep stale-lock unlock, format, resize, delete, and OverlayFS mount application in their separate
explicit mutation classes. Parser fixtures grant zero mutation authority.

#589's normal project-filesystem correlation-proof constructor stays absent until this physical
receipt is reviewed and #565 P2 becomes its sole production minting path.
