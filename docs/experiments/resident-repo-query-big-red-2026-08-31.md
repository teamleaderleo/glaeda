# Resident repository query: Big Red controlled result

Status: measured implementation candidate for Glaeda #975. This is a performance observation, not
publication, merge, source-validity, or result-reuse authority.

## Slice

`glaeda-repo-query` implements one fixed `repo-query/v1` request against an explicit canonical
checkout and complete base/head commit IDs. It returns:

- canonical repository, base, head, head tree, and merge base;
- ancestry and commit count;
- bounded per-file `--numstat --no-renames` facts;
- a SHA-256 digest and byte count for the complete patch;
- the complete patch only when it fits the requested ceiling;
- profile generation, request digest, Git-process count, bytes, and elapsed time.

The process boundary clears ambient Git configuration, credentials, hooks, lazy fetch, replacement
objects, external diff/text conversion, pager, submodule recursion, and protocol access. It
reobserves checkout, Git-directory, and origin identity before returning. It accepts no refs,
arbitrary Git arguments, shell, network, fetch, checkout, mutation, publication, retention, or
result-reuse authority.

## Controlled workload

The workload reproduced the exact evidence bundle used to review Glaeda PR #974:

- machine class: `big-red-linux-x86_64`;
- repository: `teamleaderleo/glaeda`;
- base: `1cc1595a69cb08097c78164396adf6973391b277`;
- head: `68ec6914e319ef1d7822cec0e9b7e27c0447f836`;
- measured source: `b37a40482c09980b65b351bff79480ebefa680e1`;
- measured tree: `9958ff360fe13ca89c32fc56b26acbb2ee6cd863`;
- patch digest: `sha256:911821e1fcb8ce7b65d7e6ea22fb036365acea3584a1da074594da07985673e9`;
- schedule: two warmups plus ten measured runs per arm, rotating serial order;
- validator: exact equality of object coordinates, ancestry, commit count, changed-file facts,
  aggregate numstat, and normalized patch hunks;
- semantic projection digest:
  `sha256:78a4e728b4048415495c0c19e0eea052461b01f3aa65f5036ddf59cc8472eb10`;
- path-free raw receipt digest:
  `sha256:33d75f3408faf0fa235a2969729631b0547d1381184e365cfd6b366c7d9755e3`.

## Result

| arm | definition | median | p90 | model-visible bytes | logical outer calls |
| --- | --- | ---: | ---: | ---: | ---: |
| naive remote | four GitHub API requests, including duplicate compare/diff retrieval | 3431.93 ms | 3760.88 ms | 51,201 | 4 |
| optimized remote | one GitHub compare request, projected locally | 936.95 ms | 987.56 ms | 4,679 | 1 |
| direct local | six hand-composed Git commands with a scrubbed minimal environment | 10.51 ms | 11.64 ms | 4,679 | 6 |
| Glaeda | one `repo-query/v1` invocation with typed identity and metrics | 16.58 ms | 18.60 ms | 5,534 | 1 |

Against the naive remote loop, Glaeda was 206.96 times faster, reduced model-visible bytes by
89.19%, and reduced four outer calls to one. Against the optimized remote control it was 56.50
times faster.

The direct-local control is deliberately retained: the Glaeda profile was 1.58 times slower and
18.27% larger than a perfectly composed local projection. The added cost buys one stable call,
complete checkout/repository/object revalidation, bounded failure classes, and explicit profile and
request identity. Therefore the slice is useful as a resident agent front door, not as a claim that
wrapping Git makes Git itself faster.

## Ambient-context finding

Full-loop validation also found that the named verification profiles inherited the caller's
process umask. Under ambient `0002`, test fixtures became group-writable and safety checks failed;
under `0022` they passed. The profile now binds child file creation to `0022` and records that value
in plan and receipt documents. This removes one machine/procedural fact that agents previously had
to know and reproduce correctly.
