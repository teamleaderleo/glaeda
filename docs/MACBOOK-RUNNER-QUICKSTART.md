# MacBook runner quickstart

This is the short operator path for the existing `smolrunner` Lima development VM. It does not install or register a GitHub Actions runner.

Run commands from the SmolRunner checkout on the Mac.

## First-time cmux setup

Install cmux explicitly, start the existing VM, and open the workspace:

```bash
make work-cmux-setup
```

The setup command:

- installs cmux through the reviewed `manaflow-ai/cmux` Homebrew cask when missing;
- installs Lima through Homebrew when `limactl` is missing;
- starts the existing `smolrunner` Lima instance;
- opens a cmux workspace named `smolrunner` with a `Mac host` terminal and a `Lima VM` terminal.

cmux may trigger the standard macOS Automation permission prompt when the invoking terminal first asks cmux to create or select the workspace. The helper leaves cmux socket access unchanged. Keep cmux automation in its process-only mode; broad modes such as `allowAll` are outside the SmolRunner boundary.

## Everyday workspace

After cmux is installed:

```bash
make work
```

This command starts the existing VM when needed, selects the named cmux workspace when it already exists, and creates the two-terminal workspace when it is absent. cmux provides the vertical workspace sidebar, split panes, session restoration, and attention indicators.

Override the workspace name when needed:

```bash
SMOLRUNNER_WORK_SESSION=another-session make work
```

The launcher uses cmux's macOS scripting interface to find or create the human-facing workspace. Commands that rename the workspace and terminal tabs run inside cmux, so the helper never enables broad external socket control. The Lima guest receives no cmux socket, Mac filesystem mount, SSH agent, or copied credential.

## tmux fallback

The previous Mac-side tmux workspace remains available:

```bash
make work-tmux
```

This installs missing Mac-side `tmux` and Lima packages through Homebrew, starts the existing VM, and creates or reattaches to a tmux session with `host` and `vm` windows.

Useful default tmux keys use the `Ctrl-b` prefix:

- `Ctrl-b n` / `Ctrl-b p`: next or previous window;
- `Ctrl-b 0` / `Ctrl-b 1`: select the host or VM window directly;
- `Ctrl-b c`: create another Mac-side window;
- `Ctrl-b %`: split vertically;
- `Ctrl-b "`: split horizontally;
- `Ctrl-b d`: detach while leaving the session running;
- `Ctrl-b [`: enter scroll/copy mode; press `q` to leave it.

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
make vm-status   # Lima state plus host and guest Git branches
make vm-sync     # clean guest checkout: fetch main, then fast-forward only
make vm-doctor   # run machine-readable SmolRunner doctor in the guest
make vm-observe  # read-only Mac and guest resource report
make vm-stop     # graceful VM stop
```

When `make vm-doctor` runs from a cmux terminal, completion or failure also produces a fixed cmux notification. The notification contains only the bounded result and exit status; doctor output remains in the terminal.

The full wrappers are also available directly:

```bash
bash scripts/macbook-workspace.sh help
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
- cmux remains a human-facing view and never becomes privileged execution transport;
- no project bootstrap, runner registration, automatic update, or production deployment authority is implied by these shortcuts.

Repository-specific dependency installation and verification remain repository-owned. SmolRunner will eventually provide the surrounding one-command host, runner, release-channel, rollback, and disposable-execution lifecycle after the reviewed preparation and registration paths exist.
