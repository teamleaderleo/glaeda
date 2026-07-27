# Personal worker store writer contract

## Supported writer boundary

`UnixPersonalWorkerStore` provides durable revision-guarded publication for **lock-following writers**. Every supported mutation path opens the fixed `store.lock` file and acquires its nonblocking exclusive `flock` before it reads the current document, validates an expected revision, stages a successor, publishes it, or performs recovery.

The supported deployment has one broker-owned writer authority. Code that mutates personal-worker state must use the `PersonalWorkerStore` API and must not write, rename, link, or unlink objects inside the `personal-worker` directory directly.

Within that boundary, `replace_if_revision` guarantees that:

1. the persistent writer lock is held;
2. the currently published canonical document has the caller's exact expected store revision;
3. the replacement is the exact next store revision and queue generation with the required retained history;
4. the staged document is privately created, fully written, synchronised, and revalidated;
5. publication uses an atomic same-directory rename followed by directory synchronisation.

`create` and `recover` use the same persistent writer lock. Lock contention is reported immediately as the bounded `Busy` error and never waits.

## Deliberate non-guarantee

This store does **not** claim a filesystem namespace compare-and-swap against arbitrary same-owner code that bypasses `store.lock` while retaining direct write authority over the store directory. Portable Unix `flock` is advisory, and the final same-directory rename cannot atomically compare the destination inode with an earlier opened inode.

A process that can directly replace `current.json` after validation has already crossed the reviewed writer-authority boundary. The state directory and its owner account must therefore be treated as broker-private mutation authority, not as a security boundary between mutually hostile processes sharing that account.

Direct replacement completed before a supported mutation begins is still detected by the exact revision check. In-flight direct namespace mutation by a lock-bypassing same-owner process is out of contract and must not be described as replacement-resistant CAS.

## Integration requirements

- Keep the broker as the sole holder of store-directory write authority.
- Route typed state mutations through `PersonalWorkerStore`.
- Do not expose the store directory as a generic agent workspace or plugin write target.
- Do not stack a transaction layer that bypasses the store lock or directly publishes state files.
- Preserve bounded canonical documents, private modes, no-follow opens, revision/history validation, staged-file recovery, and directory synchronisation.

The focused lock-contract tests cover contention for all supported mutation paths and detection of an out-of-band replacement that completed before lock acquisition.
