# Codex exact-commit handoff experiment

Date: 2026-07-26

Related issue: #150

## Outcome

A formatting-only commit created and tested inside the uncredentialed Lima runner was exported as an exact Git bundle, copied to the credentialed macOS host, imported, re-verified, and pushed fast-forward-only to the target branch without recreating or squashing the commit.

- Repository: `teamleaderleo/codex`
- Target branch: `fix/code-mode-live-session-summary`
- Expected parent: `4263facaf3c7d30b26cae33fd1e679278ac02105`
- Published SHA: `73e5b9fc28de0815975fad3c3d70a6a0b38399b1`
- Runner: Lima instance `smolrunner`
- Runner workspace: `/home/lima/codex-orphan-integration`
- Publisher: credentialed macOS host
- Publication mode: fast-forward only
- Squash/recreation: none

## Verified allowlist

The recovered commit changed only:

```text
codex-rs/core/src/tools/code_mode/mod.rs
codex-rs/core/src/unified_exec/process_manager.rs
```

The final runner receipt reported:

```text
PUBLISHED_SHA=73e5b9fc28de0815975fad3c3d70a6a0b38399b1

git status --short
(no output; clean)

git diff --check
(no output; passed)
```

GitHub independently confirmed the published commit is one commit ahead of the expected parent and contains only the two approved paths.

## Workflow exercised

1. Start or reuse the Lima runner.
2. Verify origin, target branch, expected remote parent, and clean workspace.
3. Recover the unique matching commit from `HEAD`, reflogs, or reachable objects using parent, subject, and changed-path constraints.
4. Verify `git diff --check` in the runner.
5. Create a temporary Git ref and bundle containing the exact commit object.
6. Copy the bundle from Lima to the Mac without copying publisher credentials into the VM.
7. Fetch the target branch on the Mac and ensure it still points to the expected parent.
8. Import the bundle and verify exact SHA, parent, and changed paths.
9. Push the imported SHA directly to the target branch.
10. Fetch the published branch back into the runner and emit final status and whitespace checks.

## Failure found during prototyping

The first wrapper embedded guest shell code inside a host command substitution. Host-side `set -u` expanded a guest-only `${origin}` variable and stopped with `origin: unbound variable` before any mutation.

The corrected implementation copied a standalone helper script into the runner and executed it with an explicit environment. This avoids mixed host/guest expansion and should be the default design for future transport-backed actions.

## Product implications

A first-class SmolRunner implementation should:

- separate plan, export, transfer, import, and publish phases;
- use structured receipts instead of parsing free-form shell output;
- preserve commit and tree identity end to end;
- keep publisher credentials outside the runner;
- enforce expected-parent and changed-path policy before export and again before publish;
- support dry-run and explicit publish confirmation;
- clean temporary refs, bundles, and helper artefacts;
- return the final remote SHA plus runner workspace status and repository checks.

This experiment provides one successful real-world proof for the core acceptance case in #150.
