# Dedicated local Actions listener

This document defines the operator-approved R1 bridge between GitHub Actions and the bounded local verification path tracked in #288.

The listener lives inside the existing ARM64 Ubuntu Lima guest. It runs as the dedicated Linux account `smolrunner-runner`. Repository build and test commands do not run directly on that listener account; the manual canary invokes the separately reviewed rootless-Podman verification wrapper.

## 1. Prepare the dedicated account

Use the checked-in manifest:

```text
examples/local-ci-runner.yml
```

It declares:

- repository `teamleaderleo/smolrunner`;
- runner user `smolrunner-runner`;
- custom label `smolrunner-local-arm64`;
- fork execution denied;
- operator-triggered trust policy;
- initial 2 CPU / 2 GiB / 768 PID job envelope.

From the Lima administrator account, use SmolRunner's existing host planning/preparation path to create and reconcile the dedicated runner account, subordinate UID/GID ranges, home, and linger state. Follow the exact confirmation printed by each current `host prepare` plan; do not hand-create a competing account or subordinate-ID allocation.

After the account exists, switch into it with an ordinary login session so `/run/user/<uid>` and the rootless Podman user session exist. The listener helper refuses uid 0, the `sudo`, `docker`, `lxd`, and `root` groups, missing subordinate IDs, an invalid runtime-directory owner, a privileged Podman socket, or a Podman runtime that cannot prove rootless mode.

## 2. Select one exact official runner release

GitHub publishes Linux ARM64 runner archives under this fixed form:

```text
https://github.com/actions/runner/releases/download/v<VERSION>/actions-runner-linux-arm64-<VERSION>.tar.gz
```

The release metadata also publishes a SHA-256 for that archive. Record both the exact version and Linux ARM64 SHA-256 before installation. The helper deliberately has no `latest` resolution logic.

GitHub's self-hosted-runner documentation allows automatic runner updates to be disabled during registration with `--disableupdate`. This lane does so because the binary identity used by the persistent MacBook worker should advance only through an explicit reviewed update. GitHub still requires self-hosted runner versions to be kept current enough to receive jobs, so the pinned version becomes ordinary maintenance work rather than an indefinite freeze.

## 3. Install the exact package

As `smolrunner-runner`:

```bash
bash scripts/local-actions-runner.sh check
bash scripts/local-actions-runner.sh install \
  --version '<VERSION>' \
  --sha256 '<LINUX_ARM64_SHA256>'
```

The install path is fixed below `/home/smolrunner-runner`. The helper:

- downloads only from the official `actions/runner` release URL derived from the exact version;
- starts network/runtime probes from a scrubbed environment;
- verifies the supplied SHA-256 before extraction;
- rejects absolute or parent-traversing archive paths;
- refuses archive-contained registration state or credentials;
- verifies `config.sh`, `run.sh`, and `Runner.Listener`;
- proves the extracted listener reports the requested version;
- copies the reviewed token bridge into the private installation and records its exact SHA-256;
- records only runner version, runner package SHA-256, and token-bridge SHA-256 in the private installation marker;
- removes an unpublished partial install on failure;
- treats a different pre-existing installation as an explicit update/recovery case.

The helper does not run `installdependencies.sh` with privilege and does not install a system service. The Ubuntu guest must already provide the ordinary runtime dependencies required by the accepted official runner package.

## 4. Obtain a short-lived repository registration token

Use GitHub's repository runner-add flow for `teamleaderleo/smolrunner` at activation time. Keep the registration token outside files, shell history, issue comments, workflow YAML, environment files, and repository state.

Registration reads exactly one token line from standard input. The helper launches the **installed, SHA-pinned** `.smolrunner-token-bridge.sh` under the scrubbed listener environment. That bridge reads the token from stdin, validates its bounded form, exports the official runner's supported `ACTIONS_RUNNER_INPUT_TOKEN` variable in-process, clears the shell variable, and `exec`s only the fixed `/home/smolrunner-runner/actions-runner/config.sh` path.

The token therefore never appears in the helper's or bridge's command-line arguments. The upstream runner consumes and clears its secret environment input during configuration.

One operator pattern is to copy the short-lived token into a shell variable without echo and pipe it once:

```bash
read -r -s runner_token
printf '%s\n' "$runner_token" | bash scripts/local-actions-runner.sh register
unset runner_token
```

The registration is fixed to:

- repository URL `https://github.com/teamleaderleo/smolrunner`;
- runner name `smolrunner-local-arm64`;
- custom label `smolrunner-local-arm64`;
- default self-hosted/Linux/ARM64 labels retained;
- work directory `_work`;
- automatic runner updates disabled;
- no `--replace` behavior.

## 5. Start the listener manually

For the first canary, run:

```bash
bash scripts/local-actions-runner.sh run
```

`run` replaces the helper process with the official `run.sh` under a fresh allowlisted environment containing only:

- the dedicated home/user/logname;
- a fixed system PATH;
- `LANG=C.UTF-8`;
- the dedicated user's `XDG_RUNTIME_DIR`;
- the matching user D-Bus address required by rootless Podman.

Ambient SSH-agent, GitHub, cloud, browser, password-manager, proxy, Docker/Podman remote-endpoint, and arbitrary shell variables are absent from the listener environment.

The first slice intentionally has no systemd service. Keep the terminal/session open while running a canary. W06 later owns supervised service lifecycle.

## 6. Run the manual canary

After R0 and the canary workflow are merged and the exact runner appears idle/online with the expected labels, manually dispatch `Local verification canary` **from the `main` workflow ref** with one exact main-history commit.

The first canary deliberately accepts only source already reachable from the exact main control commit. Feature-branch and fork source remain outside the personal runner until a later reviewed trusted-branch policy exists.

The canary must prove:

- exact main workflow-control identity;
- exact requested source commit/tree and main-history ancestry;
- exact approved listener-side verification-wrapper blob before execution;
- the prebuilt reviewed local CI image exists;
- dependency preparation is the only networked project phase;
- fmt/check/Clippy/tests execute with network disabled inside rootless Podman;
- the source mount is read-only;
- the applied 2 CPU / 2 GiB memory / bounded swap / 768 PID envelope;
- a bounded local verification receipt;
- equivalent results for the same exact commit on the hosted reference lane.

Do not add automatic PR routing until this canary passes repeatedly and the fork/trust gate receives its own review.

## 7. Stop and remove

Stopping the foreground `run.sh` process makes this manually operated listener unavailable while preserving its installation and caches.

To unregister it, obtain a fresh short-lived removal token and pipe it to:

```bash
printf '%s\n' "$runner_remove_token" | bash scripts/local-actions-runner.sh remove
```

Removal uses the same installed, digest-checked token bridge. It unregisters the GitHub runner and retains the installed package, private cache volumes, and SmolRunner account state. Package deletion, account deletion, cache deletion, and VM deletion remain separate actions with their own ownership checks.

Inspect local state at any time with:

```bash
bash scripts/local-actions-runner.sh status
```

The public helper receipts expose the exact runner package identity, installed token-bridge identity, and registration disposition without exposing tokens, credentials, private filesystem paths, or runner credential files.
