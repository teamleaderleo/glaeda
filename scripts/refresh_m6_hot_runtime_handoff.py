#!/usr/bin/env python3

from pathlib import Path
import subprocess

BASE = "5dafaf5c1d0df9666ee4c6305844d4568c66a6aa"
PATH = Path("docs/M6_HOT_RUNTIME_HANDOFF.md")

base = subprocess.run(
    ["git", "show", f"{BASE}:docs/M6_HOT_RUNTIME_HANDOFF.md"],
    check=True,
    stdout=subprocess.PIPE,
    text=True,
).stdout

old_main = "At this snapshot, `main` is `47c4586f2a68acd1e101d0fa80bdc5b7996fc632`, merge of PR #626."
new_main = "At this snapshot, `main` is `43450cf76c6cd5f388f3d79fe112ae536697f534`, merge of PR #633."
if base.count(old_main) != 1:
    raise SystemExit("verified handoff main marker changed")
base = base.replace(old_main, new_main, 1)

marker = new_main + "\n"
delta = r'''

## Continuation delta — current after #633

This section is the newest durable continuation record. Where later historical snapshot text in this file still describes #614 as awaiting acceptance, #592 as lacking a sealed admin-producer plan, or `main` as older than #633, this section wins. The historical text remains below because its experiments, corrections, and authority rationale are still useful.

### Landed since the prior handoff snapshot

- **#614 / #592 publication candidate audit landed.** Final head `2da56b9f050cefd7dbbb67859d6fe5e65ef10621` passed Verify `32761168266` and Linux acceptance `32761168272`; merge is `13529efc2dfb873488c9f29f66c9fe46a544f2e2`. The O(N) walk is publication-time evidence only and remains outside hot task admission. It accepts retained #609 source authority plus a sealed retained staging candidate, proves safe entries/inode independence/nested-alternate absence/drift, and grants zero Git/root/freeze/marker/rename/publication authority by itself.
- **#633 / #592 pure admin producer plan landed.** Product head `c33e431b9cc3c008832a5487d66121a7f770357a` passed Verify `32763130073` and Linux acceptance `32763130047`; merge is `43450cf76c6cd5f388f3d79fe112ae536697f534`. It seals fixed `/usr/bin/git clone --bare --local --no-hardlinks`, a reviewed empty template, credentials/prompts disabled, a cleared allowlisted environment, exact source-generation + candidate-binding digests, and a future credential policy requiring freshly verified non-root `smolrunner-admin` UID/GID with supplementary groups cleared. It performs no account lookup, filesystem I/O, process spawn, UID/GID change, Git execution, root mutation, staging, marker write, rename, publication, or cleanup.
- **#623 / #588 host Lima argv hardening landed.** The sealed plan uses `limactl --tty=false shell <exact-instance> -- <guest-command...>` and retains the guest `sudo -- /usr/bin/env -i ...` boundary.
- **#626 / #592 guest-control vocabulary landed.** `PublishImmutableGitPoolGeneration` is a distinct canonical operation tag; it has no handler or mutation authority yet.

### Acceptance-gated exact heads

- **#618 / #565 P2 collector:** hardened head `a0bdf00fdcf98e7fd3484d8d8630904a1db37f64`, Verify `32760003780` green. The supplied standalone-disk directory is opened component-by-component with no-follow semantics; Linux guest `st_dev` is decoded and must equal the single exact project `mountinfo` major/minor row. The receipt still says physical ownership unresolved and cannot mint #607/#589 production correlation authority. Because the final repair is ownership-adjacent, merge waits for implementation-independent exact-head acceptance.
- **#619 / #617 bounded non-secret stdin:** head `9907b0a5df525170606937a653e23e02826f3cf2`, Linux acceptance `32760871369` and Verify `32760871416` green. <=4 KiB exact bytes share the existing deadline/output/process-group cleanup loop; writer setup and actual BrokenPipe failures kill/reap the owned group; stdin bytes are absent from argv/environment/ExecutionRecord surfaces. Because this is process/concurrency-sensitive, merge waits for implementation-independent exact-head acceptance.

### Physical and execution blockers now have explicit owners

- **#628** owns one observation-only receipt on the real operator Apple Silicon Mac / installed pinned Lima environment. It must derive the available read-only disk inventory command from installed help, directly observe the standalone-disk directory, keep raw private evidence off GitHub, and end with **YES / NO / AMBIGUOUS** for exact host-disk + attachment + resident-sandbox + guest-filesystem correlation. Missing or ambiguous state stops without create/attach/unlock/format/resize/delete.
- **#629** owns publication and command-free observation of one exact root-owned, non-task-writable Linux/aarch64 guest-control binary generation. A surviving path never blesses arbitrary bytes.
- **#631** owns the read-only Mac invocation-target proof. Only exact accepted limactl bytes/generation, private protected `LIMA_HOME`, the exact running VZ/aarch64 resident sandbox generation, and the exact accepted #629 guest binary may mint the crate-private `TrustedGuestControlInvocationTarget`.

### Current #592 transaction boundary

With #614 and #633 landed, the next privileged pool-publication executor must compose existing authority instead of inventing it:

```text
root: exact private staging envelope + retained parent authority
  -> fresh exact immutable source + smolrunner-admin account evidence
  -> seal #633 producer plan
  -> drop to verified admin UID/GID and clear supplementary groups
smolrunner-admin: execute only the sealed fixed Git plan
  -> exit
root: prove producer absence + exact retained candidate
  -> #614 candidate audit
  -> reviewed Git/object reachability proof
  -> recursive root ownership/frozen modes
  -> write exact #590 marker from the same transaction nonce
  -> #609 observe staged frozen generation
  -> no-replace descriptor-relative promotion + parent fsync
  -> #609 observe final published generation
```

The executor is high-risk privilege/process/publication work and therefore needs implementation-independent exact-head acceptance. It must never run project-sensitive Git as root or let path survival grant staging ownership.

### Current execution order

1. Get independent exact-head acceptance for #618, then run #628 on the real operator Mac. A **YES** result may feed a separate #565 P2 production-correlation implementation; **NO/AMBIGUOUS** keeps #607 sealed.
2. Get independent exact-head acceptance for #619. In parallel, implement #629 guest-control binary publication/observation and #631 invocation-target proof.
3. Build the #592 privileged admin-producer/freeze/marker/promotion transaction around merged #633 + #614 + #609.
4. Implement #580 task-private Git creation with `--reference <accepted-generation> --no-local`, exact alternate validation, origin removal, inherited exact index publication, and final non-mutating Git/source proof.
5. After #619/#629/#631 acceptance, build the #588 one-shot transport adapter from merged #612/#626 protocol plus #615/#623 invocation plan; transport loss/drift remains reconciliation debt and arbitrary Lima argv stays unrepresentable.
6. Compose current authority -> #565 correlation -> #607 all-FD mount -> #580 Git/index proof -> bounded receipt -> ready publication.
7. Run complete cold and warm resident trusted-agent edit/test loops and compare #563/#566 receipts against the ordinary private/cold fallback.

'''
if base.count(marker) != 1:
    raise SystemExit("current main insertion marker changed")
PATH.write_text(base.replace(marker, marker + delta, 1))
