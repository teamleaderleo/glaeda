# MacBook runner quickstart

This is the short operator path for the existing `smolrunner` Lima development VM. It does not install or register a GitHub Actions runner.

Run commands from the SmolRunner checkout on the Mac.

## Open the VM

```bash
make vm
```

This starts the existing `smolrunner` instance when needed and opens an interactive guest shell.

The underlying direct command remains:

```bash
limactl shell smolrunner
```

## Use one persistent terminal workspace

Install `tmux` once inside the guest:

```bash
make vm
sudo apt-get update
sudo apt-get install -y tmux
exit
```

Then use:

```bash
make vm-tmux
```

That attaches to or creates the guest session named `smolrunner`. The session survives closing the Mac Terminal window, but not stopping or deleting the VM.

Useful default tmux keys use the `Ctrl-b` prefix:

- `Ctrl-b c`: new window;
- `Ctrl-b %`: split vertically;
- `Ctrl-b "`: split horizontally;
- `Ctrl-b n` / `Ctrl-b p`: next or previous window;
- `Ctrl-b d`: detach while leaving the session running;
- `Ctrl-b [`: enter scroll/copy mode; press `q` to leave it.

## Everyday commands

```bash
make vm-status   # Lima state plus host and guest Git branches
make vm-sync     # clean guest checkout: fetch main, then fast-forward only
make vm-doctor   # run machine-readable SmolRunner doctor in the guest
make vm-observe  # read-only Mac and guest resource report
make vm-stop     # graceful VM stop
```

The full wrapper is also available directly:

```bash
bash scripts/macbook-runner-vm.sh help
bash scripts/macbook-runner-vm.sh exec -- /usr/bin/uname -m
```

Override the defaults when operating another instance or checkout:

```bash
SMOLRUNNER_VM=another-instance \
SMOLRUNNER_GUEST_REPO=/home/lima/another-checkout \
  bash scripts/macbook-runner-vm.sh status
```

## Avoid ambiguous `git pull`

The wrapper deliberately uses separate fetch and fast-forward steps:

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

Repository-specific dependency installation and verification should remain repository-owned. SmolRunner will eventually provide the surrounding one-command host, runner, and disposable-execution lifecycle after the reviewed preparation and registration paths exist.
