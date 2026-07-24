# ADR 0015: Typed privilege-lane executors

- Status: Accepted
- Date: 2026-07-25

## Context

SmolRunner plans host mutations in explicit execution lanes, but a typed plan or command constructor alone is not an execution boundary. An elevated process must not accept arbitrary programs, shell fragments, ambient root credentials, or a runner-user identity that differs from the evidence inspected earlier.

The repository already has:

- typed `LaneCommand` constructors with fixed absolute programs and argv shapes;
- root-owned executable verification;
- exact runner-account, runtime-directory, and subordinate-ID evidence;
- a shell-free `ProcessExecutor` that clears the child environment and returns bounded, redacted stdout and stderr; and
- durable journal states that can represent an action whose result became uncertain.

The missing layer is a small executor that rechecks all of those contracts immediately before process creation.

## Decision

SmolRunner provides separate Linux-only `RootLaneExecutor` and `RunnerUserLaneExecutor` types.

Both executors:

1. require the command's typed lane to match the executor;
2. revalidate the command kind, exact absolute program, argv shape, and empty outer environment;
3. require effective UID zero through bounded `/proc/self/status` evidence;
4. verify every required executable immediately before process creation; and
5. delegate only to the shell-free bounded `ProcessExecutor`.

The root executor accepts only reviewed root command kinds. Its `CommandSpec` environment must be empty, and no manifest value can choose the program or introduce a shell, `sudo`, `su`, `runuser`, or arbitrary environment variable.

The runner-user executor additionally requires a previously verified nonroot `VerifiedRunnerUser`. It rechecks UID, primary GID, canonical home, exact `/run/user/UID` runtime directory, and minimum subordinate UID/GID capacity. The command must exactly match:

- `/usr/sbin/runuser --user USER --`;
- `/usr/bin/env --ignore-environment`;
- explicit `HOME`, `USER`, `LOGNAME`, and `XDG_RUNTIME_DIR`; and
- one reviewed absolute inner program with fixed arguments.

The outer process receives an empty environment. The inner `env --ignore-environment` boundary discards any environment introduced by `runuser` or PAM before adding the four explicit runner-user values.

Successful execution returns lane metadata plus the complete bounded and redacted `ExecutionRecord`, including status, stdout, and stderr. The record is retained because later inspection authorization may depend on the exact output and command receipt. A nonzero exit remains a complete record with `success = false`; callers decide how to classify it in the durable journal.

Process-layer errors are mapped to a bounded public failure without raw operating-system text. Because output capture or waiting can fail after process creation, that failure states that host state may have changed and requires re-observation before retry.

## Consequences

- Root and runner-user commands cannot switch lanes through argv content.
- Runner-user operations do not inherit root's home, SSH agent, Git configuration, cloud variables, GitHub tokens, or container variables through this API.
- Command construction and execution each enforce the same reviewed shape, providing defense in depth.
- The full bounded process record remains available to parsers and authorization logic rather than being discarded by a lossy receipt.
- Tests inject privilege, executable, process, and runner-evidence views without creating a test-only constructor for `VerifiedRunnerUser`.
- This decision does not connect any CLI command to mutation. Durable reconciliation integration and real Linux privilege-transition tests remain separate work.
