# Protected cache-generation catalog

Glaeda needs catalog-wide generation authority before any hot Cargo target can become managed or
reclaimable. Independent per-directory records are insufficient: two different state IDs can both
claim `current` while each record remains internally valid.

`protected_cache_generation_catalog` defines the first bounded document contract for one reviewed
`cargo_target_v1` family:

- one canonical protected namespace identity;
- one positive catalog revision;
- one optional current-state pointer;
- bounded, path-free state and materialized-generation identities;
- mutually consistent `current`, `retired`, and `quarantined` lifecycles;
- explicit recovery debt that blocks correlation across the entire namespace;
- strict canonical JSON encoding and decoding;
- revision-checked, read-only state correlation.

The module deliberately creates only an empty revision-one document. Decoding bytes reports
`supplied_document_only`: a JSON document cannot grant itself cache ownership. There is no
filesystem writer, path discovery, adoption, cache scan, lease inference, reconstruction receipt,
quarantine, restore, deletion, or CLI apply path.

## Required next authority

A physical producer remains blocked until one separately reviewed slice supplies all of the
following:

1. a descriptor-bound private store with exact installation/generation identity, locking, atomic
   publication, permissions and symlink defense, durability barriers, revision conflicts and
   crash recovery;
2. a replacement-equivalence success receipt binding canonical reconstruction inputs, the exact
   plan/validator/toolchain generations, a newly materialized output identity, and the declared
   family semantic digest;
3. fresh personal-worker lease-store visibility. Until leases deliberately gain generation scope,
   any active lease on the protected namespace vetoes every generation; unreadable, corrupt,
   partially observed, or revision-mismatched lease state remains unknown;
4. a read-only adapter that joins the protected catalog, equivalence receipt, lease observation,
   and live lock/mount/open-file/process evidence into the existing pure cache inventory. Missing or
   conflicting evidence must remain `unknown`.

Current Big Red Cargo targets remain unmanaged/unknown. This document grants no mutation or cleanup
authority and does not change the existing `supplied_observation_only` cache report.
