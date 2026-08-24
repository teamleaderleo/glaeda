# P4 project-disk format and first attachment lifecycle

Parent: #565
Depends on: #565 P1/P2/P3
Blocks: #628 full physical correlation rerun
Related: #560, #588, #589

## Goal

Take one genuinely SmolRunner-owned raw project-disk generation from P3 through one explicitly authorized filesystem format and one separately authorized writable resident-sandbox attachment, with crash-safe recovery at every response-loss boundary.

```text
owned raw disk
-> explicit format intent/checkpoint
-> exact filesystem format generation
-> post-format filesystem observation
-> exact resident-sandbox attachment generation
-> attachment/start
-> guest project mount
-> exact Linux filesystem-device observation
```

Formatting and resident attachment are separate mutation classes. An unresolved format transaction blocks attach. An unresolved attach transaction blocks format.

Disk names, Lima locks, mount failure, filesystem labels/UUIDs, filesystem contents, and surviving paths grant zero independent ownership authority. Ownership always begins with the durable P3 project-disk generation binding plus a fresh P2 descriptor-bound observation of the same physical/backing object.

## Durable model

Keep P1's resident attachment lease as the single-writer authority. Add a companion P4 materialization record:

```text
ProjectDiskMaterializationRecord
  project identity
  project-disk ID + generation
  P3 ownership/physical binding
  revision

  filesystem:
    raw
    | format_pending(FormatCheckpoint)
    | format_recovery_required(FormatCheckpoint)
    | formatted_detached(FilesystemGeneration, FormatReceipt)

  active_mutation:
    none
    | format(format_transaction_id)
    | attach(attachment_transaction_id)
```

`active_mutation` is revision/CAS guarded. Recovery continues the same transaction ID; it never allocates a second mutation merely because a response was lost.

## Exact filesystem generation

Choose the complete filesystem generation before `mkfs`:

```text
FilesystemGeneration
  schema version
  project identity
  project-disk ID + generation
  filesystem generation
  filesystem kind: ext4 | xfs
  controller-chosen filesystem UUID
  whole-device = true
  feature-policy digest
  expected logical bytes
  formatter binary generation + SHA-256 + version + architecture
  formatter config digest
  formatter guest generation
  canonical format-plan digest
  durability-policy generation
```

V1 uses a whole-device filesystem with no GPT/partition layer. Ext4/XFS defaults that can affect the resulting filesystem belong in the accepted formatter configuration. XFS requires an exact formatter generation containing the accepted xfsprogs/mkfs.xfs and later exact feature observation, including reflink policy where selected by #560.

The filesystem UUID is format-outcome evidence only. It never adopts or owns a disk.

## Format transaction

Use a dedicated trusted formatter sandbox generation with project code absent, exactly one additional project disk, Lima auto-format disabled, and project automount disabled. The formatter sandbox is a temporary writable holder internal to the format transaction and does not consume a resident P1 attachment generation.

Lifecycle:

```text
owned_raw_detached
-> format_intent_durable
-> formatter_carrier_attached
-> exact_guest_block_device_bound
-> ready_to_mkfs
-> mkfs_issued
-> exact_filesystem_observed
-> formatter_detach_pending
-> formatted_detached
```

### Before accepting format intent

Require all of:

- exact current P3 project-disk ID/generation and durable ownership binding;
- fresh P2 physical/backing observation equals the P3 binding;
- disk detached and unused under the accepted P2/P1 classification;
- no active format/attach/transfer/unlock/resize/delete/retire mutation;
- exact immutable `FilesystemGeneration`;
- exact trusted formatter sandbox/binary/config generation;
- fixed executable and argv, empty/allowlisted environment, bounded output/deadline;
- explicit authorization for mutation class `format` on this exact disk/filesystem generation.

### Guest block-device binding before mkfs

In the dedicated formatter guest require:

```text
accepted root disk
+ exactly one configured additional project disk
+ P2 says the exact owned backing is attached to this formatter sandbox
+ exactly one eligible non-root whole block device
-> candidate project block device
```

Validate block-device type, `st_rdev`, logical bytes, root-device exclusion, zero current mount, and the installed-Lima correlation evidence established by physical acceptance. Capacity equality is consistency evidence only.

### Last safe boundary

Immediately before spawning `mkfs`, re-prove:

- same current format checkpoint/revision;
- same P3 ownership and P2 physical/backing binding;
- exact formatter sandbox is the current writable holder;
- exact formatter sandbox generation is still running;
- exact guest block device rebinds to the same observation;
- zero mount on the device;
- expected pre-format blank/unformatted classification;
- exact formatter binary/config still match.

Only then issue the one exact mkfs request tied to the format transaction/request digest.

### Post-format observation

While the exact device remains bound, observe read-only and require:

- exact filesystem type;
- exact preselected filesystem UUID;
- exact accepted feature policy;
- exact whole-device geometry;
- no unexpected partition layer;
- reviewed durability barrier complete.

Revalidate P2 host physical/backing identity afterward. Allocated host bytes may grow; logical bytes and physical binding must remain exact.

Then stop/detach the exact formatter sandbox and require a fresh P2 observation of the same physical/backing identity in detached/unused state before committing `formatted_detached`.

## Format response-loss recovery

- Loss before formatter start: fresh observation may continue the same transaction.
- Loss around formatter start/attach: observe P2 + exact formatter sandbox before any retry.
- Loss before mkfs issuance: rerun every pre-mkfs proof.
- Once `mkfs` may have been issued: enter `format_recovery_required`; automatic mkfs replay is forbidden.
- Loss after possible mkfs completion: inspect the exact correlated block device. Exact requested type/UUID/features proves the original format completed.
- Partial/mismatched filesystem after possible mkfs: keep disk out of resident attach. A new explicit recovery-format intent is required before overwriting it.
- Loss during formatter stop/detach: P2 decides whether the formatter still holds the disk or it is already detached.
- Stale formatter lock routes through the separately authorized #565 unlock lifecycle bound to this exact transaction/carrier.
- Loss after durable `formatted_detached`: return the existing receipt; perform zero physical replay.

## Resident attachment transaction

Resident attachment starts only from committed `formatted_detached` plus P1 `detached`.

Durably reserve the next monotonic attachment generation before the first resident start request:

```text
AttachmentCheckpoint
  attachment transaction ID
  starting lease revision
  project-disk ID + generation
  filesystem generation
  reserved attachment generation
  resident sandbox ID + generation
  resident sandbox config digest
  mount-policy generation
  stage
```

Target sandbox configuration must already name exactly this project disk locator with Lima formatting and project automount disabled.

### Before resident start

Require:

- P4 materialization = `formatted_detached`;
- exact `FilesystemGeneration` accepted;
- P1 lease = detached at the current revision;
- predecessor writable attachment proven absent;
- next attachment generation durably reserved;
- fresh P2 physical/backing observation equals the P3 binding;
- P2 live use = unused and accepted lock classification permits attach;
- exact target resident sandbox generation accepted and stopped;
- exact target sandbox configuration generation;
- no competing project-disk mutation.

### After start: publish P1 attachment before guest mount

After start returns, or recovery observes its effect, require:

- same P3/P2 physical binding;
- P2 reports the exact disk attached to the exact target sandbox;
- exact descriptor-bound resident sandbox generation is Running;
- reserved attachment generation is still current;
- no foreign/competing use.

Then call the normal P1 attachment-success transition for that reserved lease.

At this point P1 truth may be `attached` while P4 guest mount is `needs_mount`. Mount trouble must never trigger a second VM attachment generation.

## Guest project mount

Before mount mutation require:

- current exact P1 attachment lease;
- exact resident sandbox generation;
- fresh P2 current-attachment observation;
- exact guest block device bound to that attachment;
- filesystem type/UUID/features equal the accepted `FilesystemGeneration`;
- exact protected project mountpoint currently unmounted;
- fixed reviewed mount-policy generation.

V1 mount policy should use fixed `rw,nodev,nosuid` semantics with any additional accepted options coming only from the reviewed policy generation.

After mount require the exact device correlation:

```text
project block device st_rdev major:minor = D
mountinfo row for exact project mountpoint major:minor = D
stat(project mountpoint).st_dev major:minor = D
mountinfo filesystem type = accepted ext4/XFS
```

Re-observe the accepted filesystem generation on the mounted filesystem and revalidate current P1 attachment + resident sandbox before accepting the final receipt.

## Attachment response-loss recovery

- Loss after attach intent, before start: fresh pre-start proof may continue the same reserved generation.
- Loss around resident start: observe exact P2 + sandbox state before retry.
- Exact physical attachment observed before P1 publication: reconcile by publishing the already-reserved P1 attachment generation after fresh exact post-observation.
- Loss after P1 attached publication: return/continue that same attachment generation.
- Loss before guest mount: resume mount phase with zero new attachment generation.
- Loss around mount request: observe block device + mountinfo + mountpoint `st_dev` first. An exact existing mount continues to final proof.
- Mount absent after a lost request may be retried only after proving the prior helper execution terminal and the same P1 attachment still current.
- Foreign/different mount or failed device correlation produces explicit revalidation/cleanup debt; it authorizes no speculative unmount.
- Host reboot while attached uses the existing P1 revalidation/unlock rules from fresh observation.

## Authority hierarchy

```text
ownership
  P3 durable project-disk generation
  + fresh P2 physical/backing identity

format
  ownership
  + durable FormatCheckpoint
  + exact formatter generation
  + exact correlated guest block device

resident attach
  ownership
  + current P1 lease revision
  + durable AttachmentCheckpoint
  + exact resident sandbox generation

filesystem outcome
  exact type/UUID/features only after device correlation

guest device correlation
  P2 host attachment
  + exact sandbox
  + block-device dev_t
  + mountinfo
  + mountpoint st_dev
```

Disk names and Lima locks may locate/classify/veto. Filesystem metadata verifies format outcome. None independently grant ownership or adoption.

## Physical Mac acceptance plan — execute later, never as ordinary CI

After P1/P2/P3/P4 implementation and exact-head high-risk review:

1. Record exact SmolRunner head, P3 disk generation/physical binding, installed Lima/help-derived CLI generation, formatter guest/binary/config generation, kernel, filesystem generation, resident sandbox generation, and mount policy.
2. Obtain a separate explicit operator approval for the `format` mutation class.
3. Execute one fresh genuine P3-owned raw disk through formatter attachment, exact guest block-device binding, one mkfs, exact post-format observation, durability barrier, formatter detach, and fresh P2 `formatted_detached` observation.
4. Exercise crash/response-loss points on fresh dedicated acceptance generations, especially loss after mkfs may have been issued; prove zero automatic duplicate format.
5. Obtain a separate explicit operator approval for the `attach/start + project mount` mutation class.
6. Reserve the first resident attachment generation, start the exact sandbox with auto-format/automount disabled, prove the host attachment, publish P1 attached, bind the guest block device, mount the exact project root, and prove `st_rdev == mountinfo == st_dev` major/minor.
7. Exercise response-loss after attach intent, start request, host attachment observation, P1 publication, mount request, and guest-device observation; also exercise one host-reboot revalidation path.
8. Route any stale Lima lock through the existing separately authorized unlock mutation. Preserve foreign/ambiguous state for diagnosis.
9. Leave one exact genuine project disk attached and mounted in one exact resident sandbox, with project/task execution still sealed, for #628's observation-only rerun.

## Exact evidence P4 must hand #628

```text
logical:
  project identity
  project-disk ID + generation
  current project-disk revision

P3:
  durable ownership binding
  exact physical-disk binding
  exact backing binding

filesystem:
  filesystem generation
  filesystem kind
  controller-chosen filesystem UUID
  feature-policy digest
  formatter generation
  accepted post-format receipt

P1 attachment:
  attachment generation
  current attached lease revision
  resident sandbox ID + generation

current Mac observation:
  descriptor-bound disk/backing still equals P3
  current physical attachment = exact resident sandbox
  exact resident VZ host identity confirmed
  disk/backing/attachment rebind checks passed

current guest observation:
  exact project block-device observation
  block-device st_rdev major:minor = D
  accepted filesystem generation observed on that device
  exact private project mountpoint
  stat(project mountpoint).st_dev major:minor = D
  exact mountinfo row major:minor = D
  mountinfo filesystem type = accepted filesystem kind
```

#628 then independently recaptures the observation-only receipt. The required correlation chain is:

```text
host backing object
-> P3 physical binding
-> project-disk ID/generation
-> P4 filesystem generation
-> P1 attachment generation
-> exact resident sandbox ID/generation
-> exact guest whole-block-device dev_t
-> exact mounted project filesystem st_dev
```

A #628 `YES` on that chain is the evidence required before production project-filesystem correlation-proof minting for #589.