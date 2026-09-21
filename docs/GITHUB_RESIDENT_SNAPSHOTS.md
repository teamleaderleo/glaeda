# GitHub resident-node advisory snapshots

Status: v1 bounded read-only contract for issue #970. This surface publishes routing/status evidence only. Glaeda performs fresh local source, profile, admission, reuse, ownership, and execution checks after any dispatch request.

## Consumer question

A GitHub-connected consumer should be able to answer, with one status read plus one slowly changing trust read:

- which reviewed Glaeda nodes have fresh evidence;
- OS/architecture class and exact Glaeda generation;
- which reviewed workload/profile generations are advertised;
- coarse capacity/pressure and a bounded active-work count;
- which explicitly allowlisted repositories and exact Git objects are resident;
- whether project state is `cold`, `warm`, `resident_hot`, or `revalidation_required`;
- whether a related request is `queued`, `preparing`, `running`, `terminal`, or `superseded`;
- the bounded receipt reference for terminal/recent-compatible work where one exists.

Every positive answer is advisory. `available`, `resident_hot`, a recent receipt, or a running request creates zero execution, dispatch, host-selection, result, cleanup, merge, ownership, or reuse authority.

## Chosen GitHub publication model

Use one bounded status branch in the existing Glaeda repository:

```text
main
  config/github-resident-snapshot-trust.json

glaeda-status/v1
  resident-nodes.json
```

`main` owns the reviewed repository allowlist and node public keys. `glaeda-status/v1` contains only the current aggregate fleet snapshot. Each node entry is independently signed with a dedicated node-local Ed25519 SSH signing key, so a writer can merge concurrent node entries without gaining authority to manufacture another node's state.

This model was selected from the issue #970 candidates:

| Model | Read cost | Writer/concurrency behavior | v1 disposition |
| --- | --- | --- | --- |
| Dedicated status repository | extra repository/trust bootstrap plus a second repository read | clean isolation | defer until the extra repository earns measurable value |
| Bounded status ref/file in Glaeda | trust on `main`, one small current fleet file, no source-branch churn | optimistic non-force push with independent signed node entries | **selected** |
| Dispatch/request surface | can reuse request transport | mixes advisory fleet evidence with mutation/request lifecycle | keep separate from #967/#1050 request authority |

The status branch is replaceable convenience state. Glaeda execution truth remains in the owning local/request/result contracts.

## Reviewed trust document

`config/github-resident-snapshot-trust.json` is small and changes through ordinary reviewed repository changes:

```json
{
  "document_type": "glaeda-github-resident-trust",
  "schema_version": 1,
  "repositories": ["teamleaderleo/glaeda"],
  "nodes": [
    {
      "id": "node-0123456789abcdef",
      "key_id": "key-0123456789abcdef",
      "ssh_public_key": "ssh-ed25519 AAAA...",
      "os_class": "linux",
      "architecture_class": "x86_64"
    }
  ]
}
```

The committed v1 file intentionally enrolls zero physical nodes. Enrolling a node requires a reviewed opaque node ID plus its dedicated public signing key. The private key stays on the producer and is never stored in the repository, snapshot, receipt, logs, request input, or consumer view.

Bounds:

- at most 8 nodes;
- at most 8 repository identities;
- public node IDs are opaque `node-` plus 16 lowercase hex characters;
- key IDs are opaque `key-` plus 16 lowercase hex characters;
- only Ed25519 OpenSSH public keys are accepted.

## Signed node schema

Canonical JSON uses sorted keys, compact separators, UTF-8, and one trailing newline. One signed node entry is capped at 16 KiB. The aggregate fleet file is capped at 128 KiB and 8 nodes.

```json
{
  "document_type": "glaeda-github-resident-node-snapshot",
  "schema_version": 1,
  "payload": {
    "authority": {
      "advisory_only": true,
      "authorizes_dispatch": false,
      "authorizes_execution": false,
      "authorizes_host_selection": false,
      "authorizes_cleanup": false
    },
    "freshness": {
      "snapshot_sequence": 42,
      "observed_at": "2026-09-21T10:00:00.000Z",
      "published_at": "2026-09-21T10:00:01.000Z",
      "producer_generation": 7,
      "maximum_useful_age_seconds": 300
    },
    "producer": {
      "glaeda_generation": "sha256:...",
      "key_id": "key-0123456789abcdef"
    },
    "node": {
      "id": "node-0123456789abcdef",
      "os_class": "linux",
      "architecture_class": "x86_64",
      "glaeda_node_generation": 4,
      "availability_class": "available",
      "pressure_class": "low",
      "capacity_class": "available",
      "active_work_count": 0
    },
    "profiles": [
      {
        "id": "verify-focused/v1",
        "class": "verify_focused",
        "generation": "sha256:..."
      }
    ],
    "projects": [
      {
        "repository": "teamleaderleo/glaeda",
        "source": {
          "commit_oid": "0123456789012345678901234567890123456789",
          "tree_oid": "0123456789012345678901234567890123456789"
        },
        "heat_class": "resident_hot",
        "verification_profiles": ["glaeda.required", "verify-focused/v1"],
        "dependency_build_state_class": "resident_generation",
        "active_task_count": 0,
        "recent_compatible_receipt_ref": "sha256:..."
      }
    ],
    "requests": [
      {
        "request_id": "req-0123456789abcdef",
        "source": {
          "repository": "teamleaderleo/glaeda",
          "commit_oid": "0123456789012345678901234567890123456789",
          "tree_oid": "0123456789012345678901234567890123456789"
        },
        "profile": "verify-focused/v1",
        "state": "running",
        "terminal_receipt_ref": null,
        "elapsed_class": "10s_to_1m",
        "estimate_class": "under_10s"
      }
    ]
  },
  "signature": {
    "algorithm": "sshsig-ed25519",
    "key_id": "key-0123456789abcdef",
    "namespace": "glaeda-github-resident-node-snapshot-v1",
    "value": "-----BEGIN SSH SIGNATURE-----\n..."
  }
}
```

### Node classes

The current owned-Linux admission observer maps to remote classes without publishing host measurements:

| Local bounded observation | Availability | Pressure | Capacity | Active count |
| --- | --- | --- | --- | ---: |
| `ready / compatible` | `available` | `low` | `available` | 0 |
| `wait / reserved` | `available` | `unknown` | `reserved` | 1 |
| `wait / node_held` | `held` | `unknown` | `unknown` | 0 |
| `wait / node_draining` | `draining` | `unknown` | `unknown` | 0 |
| `wait / pressure_high` | `available` | `high` | `insufficient` | 0 |
| `wait / capacity_unavailable` | `available` | `unknown` | `insufficient` | 0 |
| `refused / observation_unavailable` | `unknown` | `unknown` | `unknown` | 0 |

CPU, memory, PSI, process, and reservation internals remain local. The remote consumer sees classes and a count only.

### Project classes

Projects must appear in the reviewed repository allowlist. Exact commit/tree objects may be published. The v1 heat classes are:

```text
cold
warm
resident_hot
revalidation_required
```

The existing `glaeda-owned-workstation-capability/v1` maps `resident_hot` directly and maps `resident_cold` to `cold`. A narrower local producer can supply `warm` or `revalidation_required` through the explicit project-state input after making that classification locally.

`dependency_build_state_class` is one of:

```text
cold
warm
resident_generation
revalidation_required
unknown
```

No dependency names, cache contents, cache paths, dirty filenames, package inventory, or local-only source bytes enter the projection.

### Request classes

Request IDs are opaque (`req-` plus 16–64 lowercase hex characters). Source is exact repository/commit/tree. `profile` must name one advertised reviewed profile generation. States are:

```text
queued
preparing
running
terminal
superseded
```

A terminal request requires a `sha256:` receipt reference. Other states carry no terminal receipt. Elapsed/estimate evidence is intentionally coarse:

```text
under_10s
10s_to_1m
1m_to_5m
over_5m
unknown
```

Use `unknown` unless an accepted #21-style observation supports the class. The current #1050/#1051 external request adapter can later feed this projection after it lands; v1 introduces no mailbox, retry loop, queue, or execution ledger.

## Freshness and rollback rules

Freshness uses the oldest observation contributing to a snapshot. Republishin