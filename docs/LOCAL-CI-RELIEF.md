# Local CI relief lane

This is the first deliberately narrow path for moving compilation-heavy SmolRunner verification from congested GitHub-hosted runners into the existing Apple-silicon Lima guest.

It is a manual/operator path. It does not register a GitHub Actions listener, alter workflow routing, or enable fork work on the MacBook.

## Boundary

The wrapper is `scripts/local-ci-verify.sh`. It accepts only the fixed profile `smolrunner.required-local` and an exact committed Git commit/tree pair.

Repository source is mounted read-only into a disposable rootless Podman container. The container receives:

- 2 CPUs;
- 2 GiB memory;
- 2.5 GiB combined memory+swap limit, allowing at most 512 MiB swap above the memory cap;
- 768 PIDs;
- a read-only container root filesystem;
- all Linux capabilities dropped;
- `no-new-privileges`;
- private PID and IPC namespaces;
- no Mac filesystem mount or Mac credential propagation.

Two persistent named Podman volumes hold the Cargo home and Cargo target cache. Their locations stay private. Cache contents accelerate repeated work and carry no verification authority.

The wrapper rejects a present privileged `/run/podman/podman.sock` path and clears inherited Podman/Docker remote-endpoint variables before invoking the fixed `/usr/bin/podman` binary.

## Exact source

From the clean guest checkout, obtain the immutable identities:

```bash
commit="$(git rev-parse --verify HEAD)"
tree="$(git rev-parse --verify 'HEAD^{tree}')"
```

The wrapper independently recomputes both values and refuses a dirty or untracked checkout.

## Build the toolchain image

```bash
bash scripts/local-ci-verify.sh image
```

The first image uses the official Rust 1.97.1 Bookworm image and installs the reviewed Clippy and rustfmt components. The local image ID is recorded in every verification receipt. Physical acceptance should record the resolved upstream/base image identity before this lane becomes an ordinary required check.

## Prepare dependencies

```bash
bash scripts/local-ci-verify.sh prepare \
  --commit "$commit" \
  --tree "$tree"
```

`prepare` is the only network-enabled job phase. It runs exactly:

```text
cargo fetch --locked
```

inside the rootless container and warms the private Cargo cache.

## Verify with network disabled

```bash
bash scripts/local-ci-verify.sh verify \
  --commit "$commit" \
  --tree "$tree"
```

The verification phase has `--network=none` and runs, in order:

```text
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features --offline
cargo clippy --locked --all-targets --all-features --offline -- -D warnings
cargo test --locked --all-targets --all-features --offline
```

A failing phase stops the sequence and emits a bounded JSON receipt with the exact source and image identities plus phase statuses. Raw command output stays in the terminal/log stream and is excluded from the receipt.

For a clean one-command manual run:

```bash
bash scripts/local-ci-verify.sh all \
  --commit "$commit" \
  --tree "$tree"
```

## What comes next

After this wrapper passes real Lima runs and local-versus-hosted equivalence:

1. install one official Actions listener under a dedicated guest account;
2. register one explicit custom local label;
3. start with manual/trusted dispatch only;
4. invoke this fixed wrapper from the listener rather than running repository build commands on the listener account;
5. keep fork-originated work away from the personal worker;
6. remove duplicated hosted compilation only after equivalent exact-commit results are demonstrated.

The Mac auto-admission controller tracked in #289 supplies the later `run local / queue local / overflow` decision. This wrapper remains the execution boundary underneath that policy.
