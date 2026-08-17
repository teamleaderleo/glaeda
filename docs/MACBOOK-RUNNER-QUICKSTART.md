# MacBook runner quickstart

This is the short operator path for the existing `smolrunner` Lima development VM.

## Trusted Quarry fast lane

Quarry's operator-authored private workflows may use a separate persistent fast
lane. It keeps one official GitHub Actions runner online in the existing Lima VM,
preserves its workspace and caches, and routes Quarry's shared Linux runner
variable to the unique `quarry-trusted-local` label:

```bash
make quarry-runner-install
make quarry-runner-status
```

The install is idempotent after the first registration. It starts the guest
systemd service, enables Lima startup at Mac login, and waits for GitHub to
report the exact runner online before routing jobs. The one-time GitHub token is
piped to the guest over stdin and is not placed in the host process arguments,
environment, filesystem, or output.

Every service start, stop, or uninstall first requires the guest's numeric
runner ID, fixed name, operating system, architecture, and labels to match the
exact Quarry repository registration returned by GitHub. Removal refuses an
absent VM or absent guest registration instead of guessing that cleanup already
completed.

Temporarily route common Quarry jobs back to GitHub-hosted Linux without deleting
anything:

```bash
make quarry-runner-unroute
make quarry-runner-route
```

Release the VM's memory after a burst of local work while preserving its sparse
disk, workspace, toolchains, and caches:

```bash
make quarry-runner-pause
make quarry-runner-resume
```

Pause first routes new Quarry work to `ubuntu-24.04`, removes the scheduling
label from the exact numeric runner, and confirms that drained runner is idle.
It then disables Lima login autostart, stops the guest listener, confirms systemd
made the service inactive, and gracefully stops the VM. GitHub's externally
reported online state may lag the local stop, but the scheduling label is already
absent. If a job wins the race before label removal, pause leaves the drained VM
running and can be retried after the job finishes. An already-stopped VM is
reported as unobserved rather than fabricating an exact runner receipt. Resume
reuses the existing registration and warm disk, restores the scheduling label,
re-enables login autostart, waits for the exact runner online, and routes Quarry
back locally.

The first comparable Quarry `pr-check` measurement on 2026-08-17 took 169
seconds on `ubuntu-24.04` and 26 seconds on the warm trusted runner: a 6.5x job
speedup. End-to-end workflow wall time was 173 seconds hosted versus 33 seconds
local, a 5.2x speedup. The warm and cold focused pilots completed in 21 and 63
seconds respectively. The final label-drained physical idle pause completed in
7.90 seconds and a warm resume through label restoration and exact GitHub-online
confirmation completed in 15.67 seconds; the paused check showed no remaining
Lima/VZ process. These are observed
single-run measurements, not promised service levels.

Remove the registration and autostart while deliberately preserving the VM disk,
workspace, toolchains, and caches:

```bash
make quarry-runner-remove
```

This is an explicit trusted-repository optimization, not the hostile-workload
boundary. Third-party and potentially malicious repositories must continue to
use the disposable VM lifecycle described in `DISPOSABLE_AUTOSCALING_CI.md`.

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

## Default tmux workspace

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

`make work-tmux` remains an explicit alias for this compatibility path.

## Opt-in cmux workspace

cmux remains opt-in until the launcher completes hands-on acceptance on the target Apple Silicon Mac.

Install cmux explicitly, start the existing VM, and open the workspace:

```bash
make work-cmux-setup
```

The setup command:

- installs cmux through the reviewed `manaflow-ai/cmux` Homebrew cask when missing;
- installs Lima through Homebrew when `limactl` is missing;
- starts the existing `smolrunner` Lima instance;
- opens a cmux workspace named `smolrunner` with a `Mac host` terminal and a `Lima VM` terminal.

After cmux is installed, open or select that workspace with:

```bash
make work-cmux
```

Override the workspace name when needed:

```bash
SMOLRUNNER_WORK_SESSION=another-session make work-cmux
```

cmux may trigger the standard macOS Automation permission prompt when the invoking terminal first asks cmux to create or select the workspace. The launcher uses cmux's macOS scripting interface to find or create the human-facing workspace. Commands that rename the workspace and terminal tabs run inside cmux, so the helper leaves socket access unchanged and never enables broad external control.

The Lima guest receives no cmux socket, Mac filesystem mount, SSH agent, or copied credential. cmux provides the vertical workspace sidebar, split panes, session restoration, and attention indicators while remaining a human-facing view over SmolRunner commands.

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

When `make vm-doctor` runs from a cmux terminal, completion or failure also produces a fixed cmux notification. The notification contains only the bounded result and exit status; doctor output remains in the terminal. Outside cmux, notification delivery is a silent no-op.

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

The VM is a development and field-validation environment with one optional
trusted Quarry fast lane:

- ARM64 Ubuntu, systemd, cgroup v2, Rust, and rootless Podman are working;
- GitHub-hosted runners remain the explicit fallback for Quarry's ordinary Actions workflows;
- `quarry-trusted-runner.sh install` registers one persistent official listener for the exact Quarry repository and routes only its shared Linux label;
- Lima login autostart plus the guest systemd service recover that listener without an operator password;
- no webhook, cache deletion, host-home mount, SSH-agent forwarding, or Mac credential propagation is added by the trusted lane;
- cmux remains a human-facing view and never becomes privileged execution transport;
- no production deployment authority is implied by these shortcuts.

Repository-specific dependency installation and verification remain repository-owned. SmolRunner will eventually provide the surrounding one-command host, runner, release-channel, rollback, and disposable-execution lifecycle after the reviewed preparation and registration paths exist.
