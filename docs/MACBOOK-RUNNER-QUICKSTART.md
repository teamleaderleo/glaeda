# MacBook runner quickstart

This is the short operator path for the existing `smolrunner` Lima development VM. It does not install or register a GitHub Actions runner.

Run commands from the SmolRunner checkout on the Mac.

## One-command workspace

```bash
make work
```

This command:

- installs Mac-side `tmux` and Lima through Homebrew when either command is missing;
- starts the existing `smolrunner` Lima instance when needed;
- creates or reattaches to a Mac-side tmux session named `smolrunner`;
- opens a `host` window rooted in the Mac checkout;
- opens a `vm` window containing an interactive Lima guest shell.

Re-running `make work` reattaches to the same session and recreates either standard window if it was closed. The helper does not create a missing VM, provision the guest, register an Actions listener, or copy Mac credentials into Linux.

Useful default tmux keys use the `Ctrl-b` prefix:

- `Ctrl-b n` / `Ctrl-b p`: next or previous window;
- `Ctrl-b 0` / `Ctrl-b 1`: select the host or VM window directly;
- `Ctrl-b c`: create another Mac-side window;
- `Ctrl-b %`: split vertically;
- `Ctrl-b "`: split horizontally;
- `Ctrl-b d`: detach while leaving the session running;
- `Ctrl-b [`: enter scroll/copy mode; press `q` to leave it.

Override the tmux session name when needed:

```bash
SMOLRUNNER_WORK_SESSION=another-session make work
```

## Open only the VM

```bash
make vm
```

This starts the existing `smolrunner` instance when needed and opens an interactive guest shell. The underlying direct command remains:

```bash
limactl shell smolrunner
```

`make vm-tmux` remains available for an optional tmux session inside the Linux guest. The default `make work` flow intentionally runs tmux on macOS instead.

## Everyday commands

```bash
make vm-status   # Lima state plus host and guest Git branches
make vm-sync     # clean guest checkout: fetch main, then fast-forward only
make vm-doctor   # run machine-readable SmolRunner doctor in the guest
make vm-observe  # read-only Mac and guest resource report
make vm-stop     # graceful VM stop
```

The full wrappers are also available directly:

```bash
bash scripts/macbook-workspace.sh
bash scripts/macbook-runner-vm.sh help
bash scripts/macbook-runner-vm.sh exec -- /usr/bin/uname -m
```

Override the VM defaults when operating another instance or checkout:

```bash
SMOLRUNNER_VM=another-instance \
SMOLRUNNER_GUEST_REPO=/home/lima/another-checkout \
  bash scripts/macbook-runner-vm.sh status
```

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
- no official Actions listener is registered in this Lima VM;
- no project bootstrap, runner registration, automatic update, or production deployment authority is implied by these shortcuts.

Repository-specific dependency installation and verification remain repository-owned. SmolRunner will eventually provide the surrounding one-command host, runner, release-channel, rollback, and disposable-execution lifecycle after the reviewed preparation and registration paths exist.
