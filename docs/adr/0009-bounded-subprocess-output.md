# ADR 0009: Bounded subprocess output capture

## Status

Accepted for all SmolRunner subprocess execution.

## Context

The process executor previously used `Command::output()`. That interface buffers complete stdout and stderr before returning. A hostile, broken, or unexpectedly verbose child could therefore consume unbounded host memory before a command-specific decoder applied its own document-size limit.

The Podman inspect decoder introduced in ADR 0008 accepts at most one MiB, but that check occurs after process capture. The process boundary itself needs a hard limit.

## Decision

`ProcessExecutor` captures stdout and stderr through separate pipes and reader threads. Each stream retains at most one MiB. The readers run concurrently so a child that fills both pipe buffers does not deadlock behind sequential capture.

When either stream produces more than the limit:

1. the reader reports the exceeded stream;
2. the executor terminates the direct child process;
3. both readers continue draining until their pipes close;
4. the executor waits for the child and reader threads;
5. execution returns a bounded `InvalidData` error without constructing an `ExecutionRecord`.

Child stdin is explicitly disconnected. Existing argument and environment redaction runs only after successful bounded capture. The public `CommandSpec`, `CommandExecutor`, and `ExecutionRecord` interfaces remain unchanged. Child termination tolerates the child exiting between the status check and kill request so an output overflow remains classified as an output-limit failure.

## Security consequences

- Child output cannot allocate unbounded memory in the SmolRunner process.
- Stdout and stderr are drained concurrently.
- Excess output fails closed and does not become a partial successful receipt.
- Secret redaction still applies to every successfully captured stream.
- Command-specific decoders may impose stricter limits at or below the process limit.

## Deferred work

- Wall-clock command timeouts.
- Process-group or cgroup termination for descendants that outlive the direct child.
- Streaming logs to durable storage.
- Separate limits for commands with deliberately smaller outputs.
- Retaining a bounded diagnostic prefix when output overflow causes failure.
