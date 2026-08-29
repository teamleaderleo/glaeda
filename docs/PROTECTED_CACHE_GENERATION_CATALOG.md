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

The pure module deliberately creates only an empty revision-one document. Decoding bytes reports
`supplied_document_only`: a JSON document cannot grant itself cache ownership.

On Unix, `unix_protected_cache_generation_catalog_store` can persist only that empty revision-one
document. It retains no-follow-opened installation/store descriptors, binds a canonical private
envelope to the exact installation ID, current state-root generation and namespace, validates
`0750`/`0700`/`0600` ownership and modes, serializes operations with a persistent lock, synchronizes
staged bytes, publishes with an atomic no-replace rename, and synchronizes the parent directory.
One exact abandoned create may be recovered after its private canonical stage is re-synchronized;
multiple, malformed, nonempty, mismatched or conflicting stages remain recovery-required.

The store exposes no caller-supplied publication or replacement API. A protected-store snapshot is
only persistence provenance: it grants no physical cache ownership. There is still no path
discovery, adoption, cache scan, generation transition, lease inference, reconstruction receipt,
quarantine, restore, deletion, or CLI apply path.

## Required next authority

A physical producer remains blocked until one separately reviewed slice supplies all of the
following:

1. a separately reviewed typed generation-transition API around the protected store. It must
   checkpoint recovery before changing currentness, advance the catalog revision exactly once,
   preserve binding and locking, and must not accept decoded caller-supplied generations as
   adoption authority;
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
