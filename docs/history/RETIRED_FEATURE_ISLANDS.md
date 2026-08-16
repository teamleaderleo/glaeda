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
