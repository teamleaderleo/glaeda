# Descriptor-bound Linux launcher

`src/descriptor_bound_launcher.rs` is the narrow Linux execution primitive required by issue #193 and blocked PR #183.

## Reviewed plan

A `ReviewedLinuxLaunchPlan` binds all execution inputs before launch:

- one command ID;
- one canonical absolute executable path and its exact device, inode, owner, group, and mode;
- one canonical absolute working-directory path and its exact device, inode, owner, group, and mode;
- bounded exact arguments and environment values;
- either the exact current effective UID/GID or an explicit root-to-nonroot UID/GID transition.

Plan construction performs no filesystem or process operation and grants no execution authority by itself. The executor accepts only this typed plan and exposes no separate program, cwd, argument, environment, credential, shell, or signal override.

## Descriptor selection

The executor traverses each reviewed path from `/` with descriptor-relative `openat` calls. Every component uses `O_NOFOLLOW`; the final cwd must be a directory and the executable must be a single-link regular file with execute bits. The held descriptors must match the exact reviewed identities.

After acquisition, the original path names are not used for child cwd or executable selection. The launcher verifies Linux `/proc/self/fd/<n>` aliases against the held objects immediately before spawn and gives those aliases to `std::process::Command`:

- `current_dir` resolves the held cwd descriptor alias;
- the program resolves the held executable descriptor alias;
- `argv[0]` remains the reviewed logical executable path;
- replacement or ABA restoration of either original name cannot select another object.

Both launcher-owned descriptors are opened with `CLOEXEC`. They remain available during child setup and executable lookup, then close in the new image. Other caller-owned descriptors retain their existing close-on-exec state; adapters invoking this primitive must not hold unrelated non-`CLOEXEC` authority descriptors.

Only direct ELF executables are supported. Scripts are rejected before spawn so a shebang interpreter cannot create another path-selected executable stage.

## Process semantics

- inherited environment is cleared and replaced with the exact reviewed map;
- stdin is `/dev/null`;
- stdout and stderr are captured privately and concurrently;
- each stream has the repository-wide fixed one-MiB limit;
- the child starts in a new process group;
- the launcher does not forward ambient signals;
- capture failure or output exhaustion sends `SIGKILL` to the child process group;
- normal exits from 0 through 255 and terminating signals from 1 through 255 are represented explicitly;
- an inherited-credential launch executes on the caller thread with the exact reviewed effective UID/GID;
- a root-to-nonroot launch moves the complete launch operation to a short-lived helper thread;
- the helper thread clears its supplementary group list through rustix and fails before spawn if that cannot be proved;
- the child then applies the reviewed GID and UID transition through `CommandExt` and enters the reviewed ELF with no inherited supplementary groups;
- the helper thread exits after capture, so the caller thread's credentials and supplementary groups are unchanged;
- no unsafe Rust or process-wide credential mutation is introduced.

Public JSON and `Debug` output contain no executable path, cwd path, device, inode, descriptor number, raw diagnostics, or secret command value. Errors use fixed classifications without raw operating-system text.

## Replacement, ABA, and credential coverage

Focused tests deliberately:

- replace the executable after its descriptor is held, restore the original name after spawn, and prove the held reviewed executable runs;
- replace the working directory after its descriptor is held, restore the original name after spawn, and prove the child entered the held reviewed directory;
- replace an object before acquisition and require exact-identity refusal;
- reject symlink and hard-link aliases;
- reject scripts, credential drift, unbounded output, and private evidence disclosure;
- under a root launcher, execute a descriptor-bound direct ELF that reads `/proc/self/status` and prove all real/effective/saved/filesystem UID and GID fields equal the reviewed non-root targets;
- prove the child supplementary group list is empty and the caller thread's group list is unchanged after launch.

## PR #183 refactor requirement

PR #183 must not retain its `runuser -> env -> wrapper` path chain. After this primitive is merged, that PR must plan one direct reviewed ELF launcher executable whose exact argv and environment encode the Renderprove operation. The direct launcher must perform any required runner-user transition through the typed UID/GID contract, not through a later path-based `runuser`, `env`, shell, or wrapper lookup.

This module does not itself alter PR #183 and adds no browser, container, networking, credential, deployment, publication, or generic host-command authority.
