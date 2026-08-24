# Project-disk host observation

Status: #565 P2 read-only host observer

`project_disk_host_observation` encodes only the Lima 2.2.0 standalone-disk schema physically
established by the retained #634 operator-Mac fixture. It does not execute `limactl` and carries no
disk mutation or project-filesystem proof authority.

## Accepted physical schema

The observer requires an exact private Lima home and an explicitly supplied disk directory. The
disk directory must be exactly two path components beneath that home, and its basename must equal
the validated disk locator. This is locator validation, not ownership.

Every component is opened with directory/no-follow descriptors. The observer retains and
revalidates the Lima home, disk collection, exact disk directory, unique regular backing file, and,
when attached, exact instance directory. It enumerates the disk directory from its held descriptor.
The accepted direct-entry shapes are:

- detached: exactly one current-user, single-link, mode-0600 regular backing file;
- attached: that backing file plus exactly one current-user symlink whose target agrees with the
  independently supplied Lima inventory attachment and the held instance directory.

Entry names are not public and no backing or lock filename appears in the implementation. Extra,
missing, duplicate-role, directory, or special entries fail closed. The physical identity digest
binds the locator, held disk directory, and backing identity, including the opaque backing name and
exact device/inode metadata. The locator is only one field in that descriptor-derived identity; it
cannot establish a match by itself. Allocation is deliberately not part of identity because
legitimate writes change it.

On macOS, `/var/...` is accepted only when the root alias is a root-owned symlink whose target bytes
are exactly `private/var` or `/private/var`. Its identity and target are retained and rechecked, the
physical path still uses component-by-component no-follow opens, and the followed alias endpoint
must equal the held descriptor. Arbitrary intermediate symlinks remain refused. Direct
`/private/var/...` and `/private/tmp/...` paths follow the ordinary strict path.

## Inventory boundary

The caller supplies bounded pre-captured JSON-lines output from the installed Lima 2.2.0 disk
inventory operation. The parser accepts only the exact field set seen during #634, selects one exact
disk locator, and checks the reported directory, raw backing size/format, detached fields, or
attached instance fields. It does not execute a remembered CLI form.

Inventory and the attachment symlink are external correlation. A disagreement yields a
`conflicting` observation; it never replaces descriptor evidence or proves SmolRunner ownership.
Logical backing bytes come from the held regular file and must agree with inventory. Allocated bytes
are separately reported from host allocation blocks.

## P1 binding

An unbound `LimaStandaloneDiskObservation` is safe for fixture and P3 post-create inspection. It is
not a `ProjectDiskLeaseRecord` and grants no lease transition.

P3 pre-create checks may use `LimaStandaloneDiskAbsenceObservation`. It retains the already-existing
private Lima home and directly observed disk-collection descriptors, requires the planned basename
to be absent relative to that collection, requires strict inventory to contain no matching record,
and repeats both checks around parent rebind validation. This proves bounded absence only; the
planned name still grants no ownership or create authority.

Projection into P1's `ProjectDiskObservation` requires:

1. an expected physical identity bound to the exact P1 project, disk generation, and lease
   revision;
2. a fresh match to the held disk/backing descriptors;
3. for `current_attachment`, the exact P1 attachment and resident-sandbox generations mapped to a
   validated Lima observation request plus a still-confirmed descriptor-bound VZ host identity;
4. agreement among the held attachment symlink, Lima inventory, held instance directory, and that
   Lima source.

The projection call requires a fresh inventory receipt and revalidates every held descriptor both
before and after P1/resident binding. A previously returned observation cannot silently become
mutation evidence after a same-name replacement or attachment transition.

A physical mismatch becomes `conflicting`. An attached disk without the exact resident binding is
`other`, not current. Unreviewed or internally inconsistent lock/inventory shapes are `unknown` or a
bounded refusal. The observer does not infer stale/predecessor lock behavior that #634 did not
physically establish.

## Excluded authority

P2 adds no create, format, attach, detach, start, stop, unlock, resize, delete, repair, cleanup,
guest mount, OverlayFS, or `TrustedProjectFilesystemCorrelationProof` constructor. The #634 fixture
must be tested only through the unbound physical observer and must never be represented by a P1
lease record.

P3 may use the unbound post-create physical identity only after its separate durable pre-mutation
ownership checkpoint. Persisting that binding and executing an exact create command remain P3 work.
