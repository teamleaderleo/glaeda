# cmux Mac mini runner retirement

The direct-host cmux runner is retired. It ran repository-controlled GitHub
Actions steps as the logged-in macOS build user and retained a writable `_work`
directory between jobs. Event and repository checks cannot isolate malicious
workflow code, actions, dependencies, or generated programs from that account.

`scripts/glaeda-cmux-runner` now refuses both install plans and `--apply` before
reading a token or making any change. Do not route jobs to the `glaeda-mini`
label. Use Glaeda's disposable worker path for repository-controlled jobs: each
job must run in a fresh VM, use an ephemeral one-job runner, and destroy the VM
after the job.

## Remove an existing runner

First delete every repository variable that routes work to `glaeda-mini` and
wait for any running job to finish. Then inspect the removal plan and apply it:

```bash
scripts/glaeda-mini-setup --runner --uninstall
scripts/glaeda-mini-setup --runner --uninstall --apply
```

On a mini without `gh`, mint a one-time removal token on the operator's machine
and send it on standard input:

```bash
gh api -X POST repos/OWNER/REPO/actions/runners/remove-token --jq .token \
  | ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --uninstall --apply --token-stdin'
```

The uninstall remains receipt-bound. If deregistration cannot be confirmed, it
leaves the runner directory and receipt in place rather than hiding an active
registration. Resolve the reported blocker and repeat the uninstall.
