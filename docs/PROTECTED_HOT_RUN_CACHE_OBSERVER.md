# Protected hot-run cache observer

Status: implemented Linux capability boundary; no host installation or cleanup authority.

## Why it is separate

The ordinary command remains the default passive surface:

```text
glaeda --output json cache observe-hot-run --root <explicit-root>
```

It deliberately returns a schema-v2 partial observation when a current-user process cannot
traverse protected OverlayFS work directories. Running the ordinary development binary as root or
changing those directory modes would give far more authority than byte observation needs.

`glaeda-protected-cache-observe` is a separate minimal front door for the complete-byte case. A
checkout build is inert: it refuses before traversal. The only accepted production shape is the
exact installed path:

```text
/usr/libexec/glaeda/glaeda-protected-cache-observe
```

Every fixed parent from `/` through `/usr/libexec/glaeda` must remain a root-owned/root-group
non-symlink directory with no group/other write bit. The file must be a single-link
root-owned/root-group regular file, mode `0755`, no larger than 64 MiB, and the executing inode must
remain identical to that path throughout observation. The caller
must retain one non-root real/effective UID. Its effective and permitted Linux capability sets
must contain exactly `CAP_DAC_READ_SEARCH`; the inheritable set must be empty. Host-root, setuid,
missing-capability, extra-capability, writable, hard-linked, replaced, or development-tree
execution refuses before cache traversal.

The report binds this contract and the SHA-256 of the held executing bytes. It contains no
installation path or caller UID.

## Filesystem contract

The supplied root must be one canonical normalized absolute non-symlink directory. The observer:

- opens directories descriptor-relatively with `O_NOFOLLOW`, `O_NOATIME`, and `O_CLOEXEC`;
- requires the root, every state, directory, and metadata object to remain owned by the invoking
  UID;
- remains on the root filesystem;
- accepts only 64-lowercase-hex top-level state names;
- caps traversal at 1,024 states, 2,000,000 objects, and depth 64;
- reads no cache file, symlink target, socket, FIFO, or device endpoint;
- metadata-accounts stable same-filesystem special inodes only on this owner-bound path;
- double-stats non-directories, reopens directories, and revalidates both the held root and root
  pathname;
- refuses cross-state hardlinks, ownership drift, root/path rebinding, unsupported top-level
  shape, limit excess, and arithmetic overflow;
- writes nothing.

Its nested observation retains schema-v2 cache semantics. Complete bytes still enter the existing
classifier with ownership, generation, lease, lock, mount, process, reconstruction, quarantine,
and lifecycle evidence unknown. Every state therefore remains non-reclaimable. The outer document
has only `observation_only` authority and `mutation_performed: false`.

## Ubuntu 26.04 discriminator

Big Red has `kernel.unprivileged_userns_clone=1` and
`kernel.apparmor_restrict_unprivileged_userns=1`. Direct `unshare --user --map-root-user` failed
while writing its UID map. Bubblewrap successfully mapped the caller to namespace UID 0 and gave
the child `CAP_DAC_OVERRIDE|CAP_DAC_READ_SEARCH`, but Ubuntu's packaged `unpriv_bwrap` AppArmor
profile explicitly denied both capability uses. Kernel audit records named both denials.

Changing the packaged AppArmor profile would widen every unprivileged bubblewrap child. The
dedicated file-capability binary instead narrows elevated read traversal to reviewed code that
retains the caller UID, accepts only caller-owned cache objects, and emits only bounded path-free
metadata.

## Host installation and rollback boundary

Repository merge does not install or enable the observer. A separately authorized host-owned
installation must:

1. build and record one exact release binary digest;
2. atomically install those bytes at the fixed path with root/root ownership, mode `0755`, and one
   link;
3. set exactly file capability `cap_dac_read_search=ep` and re-observe the effective runtime sets;
4. run an isolated owned mode-000 fixture before the live read-only canary;
5. compare ordinary partial, protected complete, and privileged GNU `du` byte controls while
   proving metadata unchanged;
6. retain the exact installed digest and performance receipt.

Rollback first proves that no process executes or holds the exact installed inode, then removes
only that fixed file capability and exact installed file through the host configuration owner.
Removing the binary removes the observation capability; it never changes cache state.
