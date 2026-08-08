# ADR 0009: Bounded subprocess output capture

## Status

Accepted for all SmolRunner subprocess execution.

## Context

The process executor previously used `Command::output()`. That interface buffers complete stdout and stderr before returning. A hostile, broken, or unexpectedly verbose child could therefore consume unbounded host memory before a command-specific decoder applied its own document-size limit.

The Podman inspect decoder introduced in ADR 0008 accepts at most one MiB, but that check occurs after process capture. The process boundary itself needs a hard limit.

## Decision

`ProcessExecutor` captures stdout and stderr through separate pipes and reader threads. Each stream retains at most one MiB. The readers run concurrently so a child that fills both pipe buffers does not deadlock behind sequential capture. Every child starts in a fresh process group so bounded-abort paths can target the direct child and ordinary descendants together.

When either stream produces more than the limit:

1. the reader reports the exceeded stream;
2. the executor sends `SIGKILL` to the fresh child process group, with direct-child kill only as a fallback when group signalling itself fails;
3. both readers continue draining until their pipes close;
4. the executor waits for the child and reader threads;
5. execution returns a bounded `InvalidData` error without constructing an `ExecutionRecord`.

Child stdin is explicitly disconnected. Existing argument and environment redaction runs only after successful bounded capture. The public `CommandSpec`, `CommandExecutor`, and `ExecutionRecord` interfaces remain unchanged. Child termination tolerates the child exiting between the status check and kill request so an output overflow remains classified as an output-limit failure.

Commands that require a wall-clock bound use the separate `TimedCommandExecutor` contract. Its production implementation accepts only a nonzero timeout of at most 24 hours, measures one monotonic deadline from before spawn through direct-child termination and output capture, terminates the fresh process group when that deadline expires, drains and joins both capture readers, reaps the direct child, and returns `TimedOut` without an `ExecutionRecord`. An earlier output or capture abort retains its primary failure classification while cleanup completes. Existing `CommandExecutor` callers remain untimed until they opt into the explicit trait.

## Security consequences

- Child output cannot allocate unbounded memory in the SmolRunner process.
- Stdout and stderr are drained concurrently.
- Excess output fails closed and does not become a partial successful receipt.
- Timed callers cannot report success after the reviewed deadline.
- Ordinary descendants in the fresh process group are terminated on timeout or output failure, including descendants that retain the capture pipes after the direct child exits.
- Secret redaction still applies to every successfully captured stream.
- Command-specific decoders may impose stricter limits at or below the process limit.

## Deferred work

- Authoritative cgroup ownership, cancellation, graceful escalation, emptiness proof, and cleanup evidence from issue #205.
- Detecting or preventing an actively hostile descendant from escaping the process group by creating a new session; timed execution is currently for reviewed system tools, not untrusted repository programs.
- Streaming logs to durable storage.
- Separate limits for commands with deliberately smaller outputs.
- Retaining a bounded diagnostic prefix when output overflow causes failure.
