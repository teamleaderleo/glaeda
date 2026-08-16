# Retired feature islands

SmolRunner keeps retired implementation history in Git rather than compiling or copying unused
source into the current product. This ledger records exact recovery coordinates and why each island
left the active tree.

## Exact-commit handoff and runner export

- Recovery base: `b2015cd32e632ae3c27b2abbcf635a16b2c8b38a`
- Former paths: `src/exact_commit_handoff.rs`, `src/runner_export.rs`, and
  `src/runner_export/tests.rs`
- Product track: deferred personal-worker B05/B06 experiment
- Authority removed: a credentialless Git subprocess adapter that could create and inspect bounded
  exact-commit bundle exports

The modules had no CLI, service, or disposable-worker caller. Their only live references were their
public library exports, their own tests, and the implementation map. SmolRunner now delegates source
checkout and job semantics to GitHub Actions inside each disposable VM, so retaining this alternate
host-side transfer path increased compile, lint, test, and review surface without advancing the
current product boundary.

If a future design needs this behavior, recover the exact reviewed source from the named base and
reintroduce it through a new product decision and normal security review. Do not treat this ledger as
execution or ownership authority.

## Rootless-Podman preview planning and execution

- Recovery base: `ff0bec8c98194c6eca3f9b4aeb4aef0b90205803`
- Former paths: `src/preview.rs`, `src/podman_preview.rs`,
  `src/podman_preview_execution.rs`, `src/podman_preview_inspect.rs`, and
  `src/podman_preview_state.rs`
- Product track: deferred local preview experiment
- Authority removed: a bounded rootless-Podman subprocess adapter plus pure preview planning,
  inspection authorization, and reconciliation types

The five modules formed one internally connected island with no CLI, service, disposable-worker, or
other production caller. Their only current-tree entry points were public library exports, and their
behavior was exercised only by module-local tests. The disposable Lima/VZ worker is now the hostile
workload boundary; host-side rootless Podman previews are explicitly outside the product critical
path. The accepted preview ADRs remain in `docs/adr/` as historical design records.

The generic lease vocabulary remains active because other durable-store code uses it independently
of the retired preview implementation. If previews return as a product requirement, recover the
reviewed source from the named base and re-evaluate it against the then-current VM, credential,
network, and ownership boundaries rather than treating old types or names as live authority.
