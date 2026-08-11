# SmolRunner Runner Scale Set bridge

This is a deliberately small process boundary around GitHub's official
[`actions/scaleset`](https://github.com/actions/scaleset) Go client. The module pins exact commit
`cb0405b2d874500e75ae34eff8d582ab75956b45` through the Go pseudo-version in `go.mod`.

The bridge exists so SmolRunner does not reimplement GitHub's Runner Scale Set authentication,
message-session refresh, long polling, JIT configuration, or runner API semantics in Rust. It does
not use the client's convenience listener because that listener acknowledges a message before it
invokes lifecycle handlers. SmolRunner needs this ordering instead:

```text
poll through official client
-> return one bounded normalized message to Rust
-> persist/reconcile it under the durable SmolRunner lock
-> explicit ack from Rust
-> delete the message and acquire its available job request IDs
```

## Process contract

The binary rejects arguments, clears its inherited environment, and exchanges one bounded JSON
object per line on stdin/stdout. A bounded strict response gate rejects unknown service lifecycle
types before the pinned official client can normalize them. Protocol version 1 supports:

- `start`: accept the GitHub App private key through stdin, verify one already-enrolled exact scale
  set by ID/name/group/labels/disabled-update policy, and create a message session;
- `poll` and `ack`: expose the latest validated statistics and bounded job lifecycle messages
  without automatic acknowledgement, and accept only positive unique acquired IDs from the exact
  persisted available-job set;
- `generate_jit`: return one exact runner ID/name plus its one-time encoded JIT configuration;
- `observe_runner` and `remove_runner`: observe by exact name and remove only after re-observing the
  exact numeric ID, name, and configured scale-set identity.

Errors contain fixed codes only. The App private key never appears in argv, the environment,
stdout, or an error. The encoded JIT configuration appears only in the successful `jit` response
and remains secret-bearing data for the future Rust/guest handoff.

## Current nonclaims

This package is not yet called by the Rust controller. It does not read the Mac Keychain, create or
adopt a scale set, persist message receipts, reserve host capacity, launch a VM, transfer JIT data
to a guest, supervise a runner, or recover a bridge/process crash or ambiguous network outcome
between message deletion and job acquisition. Those remain M3 integration work. Until the Rust
adapter persists a polled message and its available request IDs before `ack`, this bridge is a
tested protocol foundation, not a usable autoscaler.
