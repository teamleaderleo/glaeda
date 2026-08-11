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
-> delete the message and acquire its available job request IDs only when Rust's exact durable
   reservation remains inside its hard execution deadline
```

## Process contract

The binary rejects arguments, clears its inherited environment, and exchanges one bounded JSON
object per line on stdin/stdout. A bounded strict response gate rejects unknown service lifecycle
types before the pinned official client can normalize them. Protocol version 1 supports:

- `start`: accept the GitHub App private key through stdin, verify one already-enrolled exact scale
  set by ID/name/group/labels/disabled-update policy, and create a message session;
- `poll` and `ack`: expose the latest validated statistics and bounded job lifecycle messages
  without automatic acknowledgement; Rust explicitly selects whether an acknowledgement may
  acquire the one persisted available job, and returned acquired IDs must remain a positive unique
  subset of that exact offered set;
- `resume`: restore a fresh session's durable acknowledged-message cursor and, when Rust has a
  pending durable message, re-fetch that exact message ID before acknowledgement can resume; Rust
  separately requires the complete normalized event bundle to equal its durable inbox;
- `generate_jit`: return one exact runner ID/name plus its one-time encoded JIT configuration;
- `observe_runner` and `remove_runner`: observe by exact name and remove only after re-observing the
  exact numeric ID, name, and configured scale-set identity.

Errors contain fixed codes only. The App private key never appears in argv, the environment,
stdout, or an error. The encoded JIT configuration appears only in the successful `jit` response
and remains secret-bearing data for the future Rust/guest handoff.

## Current nonclaims

The private Rust worker service now verifies and starts this bridge after local durable recovery;
Rust, not this process, reads the Mac Keychain reference, persists message receipts, reserves host
capacity, drives Lima, and supervises the runner. The bridge still does not create or adopt a scale
set. A crash before acknowledgement starts is recovered by exact redelivery matching. A crash or
ambiguous network result after durable acknowledgement starts remains explicit recovery debt,
because message deletion and job acquisition cannot be truthfully replayed from local evidence
alone. The composed service remains private until operator holds, bounded status, and the
`worker serve`/launchd boundary are added.
