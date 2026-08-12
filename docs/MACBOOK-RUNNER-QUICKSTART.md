# MacBook runner quickstart

This is the short operator path for the existing `smolrunner` Lima development VM. It does not install or register a GitHub Actions runner.

Run commands from the SmolRunner checkout on the Mac.

## Reviewed resource profiles

```bash
bash scripts/macbook-runner-vm.sh profile interactive  # 3 GiB / 4 vCPU
bash scripts/macbook-runner-vm.sh profile work         # 10 GiB / 8 vCPU
```

A real change uses graceful `stop -> limactl edit -> start -> verify`. Re-selecting the exact running profile is idempotent but still proves no operator run or `Runner.Worker` is active before re-verifying guest CPU and memory.

Run one explicit command under a reviewed profile:

```bash
bash scripts/macbook-runner-vm.sh run work -- /usr/bin/nproc
```

`run` forwards only the supplied argument vector through `limactl shell ... --`, reports its exit status, and leaves shutdown explicit.

The fresh-instance references are `examples/lima/smolrunner-interactive.yaml` and `examples/lima/smolrunner-work.yaml`.

## Default Mac workspace

```bash
make work
```

`make work` now chooses the richer cmux workspace when a working cmux CLI is available and falls back to the compatibility tmux workspace otherwise. The choice is capability-based; SmolRunner correctness does not depend on either UI.

With cmux available, the command:

- starts the existing `smolrunner` Lima instance when needed;
- opens or selects a cmux workspace named `smolrunner`;
- keeps a `Mac host` terminal rooted in this checkout;
- keeps a `Lima VM` terminal entering the persistent guest;
- creates and repairs those standard surfaces through the cmux CLI rather than macOS Automation scripting;
- refreshes the sidebar from fresh Lima observations instead of leaving a one-time `running` marker behind;
- shows the reviewed `interactive` or `work` profile when the VM is running and `stopped` after shutdown;
- shows an active Actions worker when the guest process observation proves one exists;
- optionally shows active and queued personal-worker counts when `SMOLRUNNER_PERSONAL_WORKER_STORE_ROOT` and a built SmolRunner CLI are explicitly supplied.

The workspace uses only documented cmux commands such as `list-workspaces`, `new-workspace`, `tree`, `new-split`, `rename-tab`, `send`, `set-status`, `clear-status`, and `select-workspace`. It does not widen cmux control-socket access, pass the socket into Lima, or scrape terminal text as runner state.

SmolRunner also leaves global cmux preferences alone. A personal terminal configuration such as terminal-kit can own fonts, themes, sidebar preferences, copy controls, keybindings, and other app settings while SmolRunner supplies only project-specific workspace content and live status metadata.

Override the workspace/session name when needed:

```bash
SMOLRUNNER_WORK_SESSION=another-session make work
```

## Explicit cmux and tmux paths

Install cmux explicitly through the reviewed cask and open the workspace:

```bash
make work-cmux-setup
```

After cmux is installed, force the cmux path with:

```bash
make work-cmux
```

Force the compatibility tmux path with:

```bash
make work-tmux
```

The tmux workspace creates or reattaches a Mac-side session named `smolrunner`, with a `host` window rooted in the checkout and a `vm` window containing an interactive Lima guest shell.

Useful default tmux keys use the `Ctrl-b` prefix:

- `Ctrl-b n` / `Ctrl-b p`: next or previous window;
- `Ctrl-b 0` / `Ctrl-b 1`: select the host or VM window directly;
- `Ctrl-b c`: create another Mac-side window;
- `Ctrl-b %`: split vertically;
- `Ctrl-b "`: split horizontally;
- `Ctrl-b d`: detach while leaving the session running;
- `Ctrl-b [`: enter scroll/copy mode; press `q` to leave it.

## Live cmux status projection

The small read-only helper:

```bash
bash scripts/macbook-ui-state.sh
```

returns bounded JSON derived from the current Lima configuration and process observation. It reports only the VM state, reviewed profile match, whether an Actions worker is observed, and whether the explicit operator-run marker exists. It does not mutate the VM or inspect arbitrary guest data.

Refresh the named cmux workspace manually with:

```bash
bash scripts/macbook-workspace.sh sync-cmux
```

The regular `make vm-up`, `make vm-status`, `make vm-sync`, `make vm-doctor`, and `make vm-stop` paths also refresh cmux metadata when the workspace exists. Each refresh first clears SmolRunner-owned status keys and then writes the fresh observation, which prevents stale `running`, worker, or queue badges from surviving later state changes.

For the durable personal-worker read model, an explicit store can be projected too:

```bash
SMOLRUNNER_PERSONAL_WORKER_STORE_ROOT=/absolute/state/root \
SMOLRUNNER_BIN=./target/debug/smolrunner \
  bash scripts/macbook-workspace.sh sync-cmux
```

That read reuses the schema-versioned `worker status` command. A missing store, missing binary, or failed read simply omits queue/activity badges; it never invents state.

## Open only the VM

```bash
make vm
```

This starts the existing `smolrunner` instance when needed and opens an interactive guest shell. The underlying direct command remains:

```bash
limactl shell smolrunner
```

`make vm-tmux` remains available for an optional tmux session inside the Linux guest.

## Everyday commands

```bash
make vm-status   # Lima state plus host and guest Git branches; refresh cmux status
make vm-sync     # clean guest checkout: fetch main, then fast-forward only; refresh cmux status
make vm-doctor   # run machine-readable SmolRunner doctor; notify and refresh cmux
make vm-observe  # read-only Mac and guest resource report
make vm-stop     # graceful VM stop; refresh cmux status to stopped
```

When `make vm-doctor` runs from a cmux terminal, completion or failure produces a fixed cmux notification. Outside a cmux terminal, SmolRunner can still flash the named workspace when it is available. Doctor output itself remains in the terminal.

The full wrappers are also available directly:

```bash
bash scripts/macbook-workspace.sh help
bash scripts/macbook-runner-vm.sh help
bash scripts/macbook-runner-vm.sh exec -- /usr/bin/uname -m
bash scripts/macbook-runner-vm.sh profile work
bash scripts/macbook-runner-vm.sh run work -- /usr/bin/nproc
```

Override the VM defaults when operating another instance or checkout:

```bash
SMOLRUNNER_VM=another-instance \
SMOLRUNNER_GUEST_REPO=/home/lima/another-checkout \
  bash scripts/macbook-runner-vm.sh status
```

## Stop while preserving warm caches

```bash
bash scripts/macbook-runner-vm.sh stop
```

A graceful stop releases the VM RAM envelope back to macOS while retaining the persistent instance disk. Runner-owned Cargo registry/Git data, `CARGO_TARGET_DIR`, repository build caches, explicitly owned package-manager caches, reviewed Podman layers, and guest checkout data survive stop/start and profile changes. The helper never deletes or recreates the instance and never prunes caches.

## Avoid ambiguous `git pull`

The VM sync helper deliberately uses separate fetch and fast-forward steps:

```bash
git switch main
git fetch --prune origin main
git merge --ff-only origin/main
```

This avoids `fatal: Cannot fast-forward to multiple branches`, which can occur when a pull resolves more than one branch or refspec.

Check where you are before changing anything:

```bash
git branch --show-current
git status --short --branch
git remote -v
```

`make vm-status` runs the branch/status check for both the Mac checkout and the guest checkout. `make vm-sync` refuses to switch or update when the guest has local changes.

## Current boundary

The VM is currently a development and field-validation environment:

- ARM64 Ubuntu, systemd, cgroup v2, Rust, and rootless Podman are working;
- GitHub-hosted runners still execute the repository's ordinary Actions workflows;
- no official Actions listener is registered in this Lima VM by the workspace helper;
- no daemon, timer, queue polling, webhook, automatic wake-up/shutdown, cache deletion, host-home mount, or credential propagation is added by these workspace shortcuts;
- cmux remains a human-facing projection and never becomes privileged execution transport or authoritative runner state;
- terminal/editor integration is intentionally an adapter over SmolRunner state, so the future `project enter` journey can hand an accepted project materialization to the same workspace surface without giving cmux project-ownership authority;
- no project bootstrap, runner registration, automatic update, or production deployment authority is implied by these shortcuts.

Repository-specific dependency installation and verification remain repository-owned. SmolRunner will eventually provide the surrounding one-command host, runner, release-channel, rollback, project-entry, and disposable-execution lifecycle after the reviewed preparation and project namespace paths exist.
