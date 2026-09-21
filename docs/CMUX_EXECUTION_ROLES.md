# CMUX execution roles and workload capabilities

Tracking issues: #1056, #1057, #1058, #546, #743, #760, #840.

This model sits between accepted CMUX fleet enrollment and placement. Enrollment establishes node identity and exact accepted generations. Role canaries establish what work a node may perform. Contention evidence establishes how much work a reviewed resource profile may admit. Placement preference stays advisory and comes from evidence.

The implementation is the pure model in `scripts/cmux_execution_roles.py`. It performs no remote scheduling and grants no execution authority.

## Closed role vocabulary

Use the repository's existing role names and keep specialization in capabilities and operations:

| Role | Platform | Typical capabilities |
| --- | --- | --- |
| `cmux_macos_native_build` | macOS arm64 | `native_xcode_build`, `native_release_build`, native Glaeda build |
| `cmux_macos_test` | macOS arm64 | `app_host_test` |
| `cmux_linux_ci` | Linux | `linux_ci`, `web_ci`, `background_verification` |
| `cmux_linux_agent` | Linux | `linux_agent`, `build_helper` |
| `artifact_cache` | cross-platform | `artifact_service` |
| `background_replay` | cross-platform | `background_replay` |
| `benchmark` | cross-platform | `benchmark` |
| `diagnostic` | cross-platform | `diagnostic` |

The draft names map into this vocabulary as follows:

- compile, release, and native developer builds use `cmux_macos_native_build` with different required capabilities;
- app-host work uses `cmux_macos_test`;
- Apple toolchain canary is acceptance/lifecycle evidence, not a routable role;
- web CI and background verification use `cmux_linux_ci`;
- build helpers use `cmux_linux_agent`.

A new specialization becomes a capability or operation first. A new role requires a distinct acceptance boundary that cannot be represented safely by an existing role.

#1056 currently has reviewed enrollment workloads for `cmux_macos_native_build` and `cmux_linux_ci`. The other names remain reserved until their exact acceptance workloads land. This model does not widen that gate.

## Role eligibility

A node is eligible for a role only when all of these agree:

1. the node is in a routable lifecycle state;
2. the platform and architecture match the role;
3. reviewed CPU, memory, and disk classes meet the role minimums;
4. required node capabilities are present;
5. a current accepted role canary exists;
6. the canary matches the current #1056 enrollment generation;
7. the canary matches the current Glaeda generation;
8. the canary matches the current #1056 per-role acceptance-workload generation;
9. the canary matches the current execution capability generation;
10. the canary accepted every capability needed by the role;
11. toolchain-bound roles bind a semantic profile to the exact accepted #1056 toolchain generation.

Enrollment alone yields zero role eligibility.

The node capability projection carries `enrollmentGeneration`, exact `glaedaGeneration`, and `roleWorkloadGenerations` from #1056 plus a separate `capabilityGeneration`. Any acceptance-relevant OS, hardware-class, Glaeda, toolchain, SDK, workload, or capability change makes the affected role canary stale. #1058 then drives the node through canary before routing resumes.

`toolchainProfiles` maps an immutable semantic name such as `apple-xcode-26-sdk-26` to the exact accepted #1056 toolchain-generation digest. A toolchain update changes that digest or publishes a new profile; the previous canary cannot authorize the changed toolchain.

## Machine capability classes

Remote placement consumes reviewed classes instead of arbitrary host controls:

- CPU: `small`, `medium`, `large`;
- memory: `small`, `medium`, `large`, `xlarge`;
- disk: `small`, `medium`, `large`;
- pressure: bounded CPU, memory, swap, and thermal classes.

The model exposes no caller-selected core counts, byte counts, cgroup settings, hostnames, or machine purchase metadata.

Platform cost, machine age, and "newest host" are absent from eligibility and preference policy.

## Workload requirement object

A workload names useful work and semantic requirements. It never names a machine.

Example:

```json
{
  "schema": "glaeda-cmux-workload-requirement/v1",
  "operation": "cmux_macos_compile_admission",
  "role": "cmux_macos_native_build",
  "platform": "macos",
  "architecture": "arm64",
  "toolchainProfile": "apple-xcode-26-sdk-26",
  "minimumCpuClass": "medium",
  "minimumMemoryClass": "medium",
  "requiredCapabilities": ["native_xcode_build"],
  "resourceProfile": "medium"
}
```

Operations currently model:

- `cmux_macos_compile_admission`;
- `cmux_macos_release_admission`;
- `cmux_macos_app_host_test`;
- `cmux_linux_ci_admission`;
- `cmux_web_ci_admission`;
- `cmux_linux_agent_admission`;
- `cmux_build_helper_admission`;
- `background_verification`;
- `artifact_cache_service`;
- `background_replay`;
- `benchmark`;
- `diagnostic`.

A scheduler may select only nodes whose current eligibility and measured resource profile satisfy the object. Local admission repeats the checks against fresh node state.

## Resource profiles and measured concurrency

The caller chooses only one reviewed profile:

`small`, `medium`, `large`, or `exclusive`.

Every accepted role/profile pair requires current capacity evidence. Profiles with no heavy lease claim still require measurement.

A `glaeda-cmux-role-capacity/v1` receipt binds:

- node, enrollment generation, Glaeda generation, capability generation, role, and resource profile;
- exact role-workload generation plus toolchain profile/generation context;
- one local slot class and measured maximum concurrency;
- `contentionEvidenceGeneration`, an immutable digest of the reviewed #760-style evidence;
- validated completions;
- p50 and p90 final-result latency;
- CPU pressure;
- memory pressure;
- swap class;
- thermal behavior where available;
- unfinished work.

#760 contention windows remain the evidence producer. This model consumes reduced evidence and does not invent concurrency from core count or RAM size. #840 native Linux measurements can therefore establish values such as four medium jobs or one heavy job only after the semantic and pressure evidence supports them.

Mixed workloads require evidence appropriate to their claims. A compile lane measured at one stays one even when a different role/profile has demonstrated higher concurrency on another lane.

## Physical lease boundary

Workload operations compile to local scarce-resource claims. Remote callers never name the claim directly.

Current claims include:

- `mac_native_build_lane`;
- `mac_app_host_test_slot`;
- `artifact_publisher_slot`;
- `linux_medium_slot`;
- `linux_heavy_slot`;
- `browser_ui_test_slot`.

The `mac_native_build_lane`, `linux_heavy_slot`, and `artifact_publisher_slot` names reuse #1057's vocabulary.

`local_admission` is `admission_only`. It reports `physical_execution_lease` as the lease boundary for work with scarce claims. The local #1057 lease owner supplies current held-slot observations and must acquire the lease before launch. This prevents two orchestrators from converting the same advisory placement into overlapping execution.

A node that becomes critically pressured after remote selection refuses at local admission before a lease is granted.

## Eligibility and preference stay separate

Eligibility comes only from exact acceptance.

Preference is observation-only evidence for #546. A node may be:

- preferred for latency-sensitive native compile;
- eligible for app-host tests;
- measured for background replay.

Changing preference never adds a role or repairs a failed canary. A preference record contains a role and evidence generation with `authority: observation_only`.

#546 may rank only among already eligible candidates and reviewed resource profiles. Cost assumptions, platform stereotypes, and machine newness are outside this model.

## Founder/operator status

The operator projection contains the decision-ready subset:

```text
node cmux-mac-001
eligible:
  cmux_macos_native_build
  cmux_macos_test
temporarily unavailable:
  artifact_cache: role_canary_pending
capacity:
  mac_app_host_test_slot: 2
  mac_native_build_lane: 1
preferred:
  cmux_macos_native_build
state: active
```

Internal enrollment/capability/workload generation objects stay out of the human view. The machine-readable form retains bounded reason codes.

## Acceptance coverage

`scripts/test-cmux-execution-roles.py` covers:

- exact role requirements;
- wrong Xcode/toolchain profile;
- insufficient CPU and memory classes;
- failed role canary;
- stale role canary after re-enrollment;
- stale role canary after Glaeda or reviewed workload generation changes;
- exact toolchain-generation change behind the same semantic profile;
- draining node;
- role invalidation after capability/toolchain upgrade;
- multiple roles sharing a scarce physical lane;
- profile-specific concurrency so another role cannot inflate a lane;
- external scheduler request for an ineligible role;
- pressure changing between selection and local admission;
- unmeasured resource profile refusal, including background work;
- stale #760 capacity evidence after an acceptance-workload generation change;
- #760-style capacity evidence fields;
- one synthetic Mac with two roles;
- one synthetic Linux node with two roles;
- preference separated from eligibility;
- compact founder/operator output;
- workload objects with no machine selector or raw CPU/RAM/cgroup controls.

Physical acceptance remains a later step. It should attach exact receipts proving at least one CMUX-owned Mac with two accepted roles and one CMUX-owned Linux node with two accepted roles, then demonstrate an incompatible workload refusal on each relevant path.
