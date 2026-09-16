# Validate infrastructure architecture audit

Date: 2026-08-27

Audited checkout: `/home/newton/work/dev-hermit/hermit`

Snapshot: clean `main` and `origin/main` at
`330f01f88eae8264ebbc89bba34b80663f9df39a`.

This audit did not modify the Hermit product repository. It used plan/list
commands and source inspection only; it did not run a full validate or an
expensive test. This report lives in the `dev-hermit` parent workspace because
it is research material rather than product documentation.

## Executive result

The current implementation is not the desired thin wrapper, but its test
inventory is measurable:

- Two shipped lane DAG files contain 70 outer nodes: 59 portable and 11
  privileged.
- Runtime profile planning yields 14 `quick`, 67 `portable-only`, 76 `full`,
  and 144 `super` outer nodes.
- The ordinary required E2E plan contains 307 cells over 240 test IDs: 305
  portable and 2 privileged.
- `full` is the only ordinary profile eligible for the hard-green receipt.
  `portable-only` is an exact outer-node subset of `full`. `quick` is a
  purpose-built smoke plan, not a literal subset. `super` is a separate stress
  plan, not `full` plus more nodes.
- The committed graph still nests dagrun. The clearest product path is the
  static `test.strict_compat` node, which invokes `validate` again; the inner
  invocation builds a roughly 193-node graph and calls the scheduler again.
- No one command currently lists the full validation denominator at every
  layer. `validate --show-plan` lists outer nodes; `test-harness plan` lists
  E2E cells; `manifest-cli list` lists manifest test definitions; Rust test IDs
  remain inside coarse DAG nodes.
- Capability selection has no single source of truth. The manifest vocabulary,
  DAG host capabilities, workflow probes, harness checks, and Rust `#[ignore]`
  decisions overlap but do not derive from one typed machine-capability result.

The shortest path to green is not to flatten or rewrite this architecture now.
The prior five red nodes are believed fixed or classified at this snapshot. A
focused phase-1 run at the exact head instead found the active blocker:
`build.runtime_release_in_pinned_root` exited 101 because the isolated LiteInst
runtime build needed uncached `foreign-types-macros` 0.2.4 while Cargo was
forced offline. Repair and prove that cache closure, continue the focused run,
then freeze `main` and launch one full validate. Do the flattening, unified
listing, capability consolidation, and external two-run comparison after
`commits-since-green` reaches zero.

## Measured inventory

### Static DAG files

Command:

```bash
find ci/dag -maxdepth 1 -name '*.json' -print -exec jq '.steps | length' {} \;
```

Result:

| File | Outer nodes |
| --- | ---: |
| `ci/dag/portable.json` | 59 |
| `ci/dag/privileged.json` | 11 |
| Total | 70 |

These are the only shipped lane DAG JSON files. The JSON files under
`agent-utils/examples/` are examples, not validation lanes.

The portable node groups are 5 build, 14 check, 2 doc, 15 E2E, 2 lint, 2 setup,
and 19 test nodes. The privileged file has 2 build, 1 check, 1 CPUID, 3 E2E,
1 PMU, 1 setup, and 2 test nodes.

### Runtime profile plans

Command, repeated for each profile:

```bash
./scripts/validate.rs PROFILE --show-plan \
  --skip-inner-dirty-working-tree-and-rebase-freshness-checks \
  --no-label-pr
```

Result:

| Profile | Outer nodes | Relationship |
| --- | ---: | --- |
| `quick` | 14 | 7 tags are unique to quick; not a literal subset of full |
| `portable-only` | 67 | exact subset of full |
| `full` | 76 | portable-only plus 9 privileged-prefixed nodes |
| `super` | 144 | separate plan; only 7 tags overlap full |

`full` adds these nine outer nodes to `portable-only`:

```text
privileged-build.manifest_guests
privileged-build.manifest_guests_in_pinned_root
privileged-build.privileged_tests
privileged-cpuid.faulting
privileged-e2e.manifest_applications
privileged-e2e.manifest_backend_parity_c
privileged-pmu.preemption
privileged-test.cli_kvm
privileged-test.pmu_buck_chaos_cases
```

`--sequential-lanes` keeps portable and privileged as two scheduler calls.
Default profile execution fuses them into one outer `DagConfig`; it does not
make the two static files two nested invocations.

### Required E2E cells

Commands:

```bash
target/debug/test-harness plan --format json
target/debug/test-harness plan \
  --lane portable --mode verify --backend ptrace --ci-only --format json
./tests/manifest-cli.rs list
target/debug/hermit-manifest-plan --format matrix-json
```

Measured result:

| Population | Count |
| --- | ---: |
| Ordinary required E2E cells | 307 |
| Unique test IDs in required cells | 240 |
| Portable required cells | 305 |
| Privileged required cells | 2 |
| `verify` cells | 298 |
| `chaos` cells | 5 |
| `custom` cells | 3 |
| `replay` cells | 1 |
| `ptrace` cells | 244 |
| `sabre` cells | 56 |
| `liteinst` cells | 6 |
| `kvm` cells | 1 |
| Quick E2E selector cells / test IDs | 235 / 235 |
| Manifest test definitions | 359 |
| Theoretical manifest matrix | 7,539 |

The theoretical matrix is 359 test definitions times 21 mode/backend
combinations. It is not the hard-green denominator; the manifests select 307
ordinary required cells from it.

The checked-in compatibility scorecard has another denominator:
`ci/compat-envelope/cells.json` contains 5,744 projected cells, with 304 green,
372 red, and 5,068 not-applicable at this snapshot. It projects 16 combinations
per test definition and omits the three required `custom` cells. Therefore the
scorecard, the required E2E plan, and the theoretical matrix must be named
explicitly rather than reported as one generic “test count.”

## Where static planning becomes dynamic

The ordinary path is:

```text
scripts/validate.rs
  -> read and transform ci/dag/{portable,privileged}.json
  -> add profile, pinned-root, preflight, retry, and publication behavior
  -> dagrun outer graph
       -> coarse build/check/test nodes
       -> manifest bucket node
            -> target/debug/test-harness
                 -> ci/manifest-plan
                      -> selected E2E cells
                           -> hermit command(s)
```

Additional committed tables expand at runtime:

- `ci/super/gates.json`: 32 rows, including 3 synthetic expansion rows.
- Compatibility corpora: 193 strict, 212 SaBRe, 174 rr, and 172 e9patch rows.
- The manifest YAML files: 13 bucket manifests plus defaults.

The static DAGs therefore describe coarse orchestration. They do not contain
the final list of Rust test IDs or E2E cells.

## Nested dagrun: where and why

`ci/dag/portable.json` defines `test.strict_compat` by running:

```bash
./scripts/validate.rs --portable-strict-compat-only \
  --reuse-parent-manifest-gate ...
```

That inner `validate` invocation generates the strict compatibility graph and
calls dagrun again. Direct planning measured 194 nodes from an ordinary host
working directory; when already inside the pinned `/test` tree, the extra
pinned-root fetch is suppressed and the inner graph has 193 nodes.

The reason recorded in `ci/dag/README.md` is reuse: the focused validate entry
point already owns corpus expansion, aggregate timeout handling, and internal
parallelism. Calling it avoided copying that logic into the static DAG. This is
a historical composition convenience, not a requirement of the test model.

Other nested scheduler paths exist:

- `validate --requalify-cell` schedules a wrapper node that invokes
  `pressure-test.rs`, which constructs and runs another `DagConfig`.
- The manifest validation gate invokes validation self-tests whose scheduler
  fixtures run inner DAGs during an ordinary production validation.

Dagrun currently accommodates nesting by removing the shared resource-cap
environment from a child scheduler to avoid self-deadlock. That also means the
outer scheduler no longer has global resource accounting for work inside the
child. Making dagrun refuse nesting by default is sensible, but it must be
sequenced with flattening these callers; landing the refusal first would turn
known graph nodes red.

Recommended shape after green: expand strict compatibility and exact-cell
requalification into the outer plan before the single scheduler launch. Keep a
temporary explicit override only for scheduler self-tests if they genuinely
need to test nested behavior.

## What can be listed and selected today

| Layer | List/plan surface | Narrow execution surface | Gap |
| --- | --- | --- | --- |
| Validate | `validate PROFILE --show-plan` | `--only lane group.job,...` | Shows outer nodes only; cannot select a Rust test ID within a node |
| E2E harness | `test-harness plan --format json` | `test-harness run --test ID --mode MODE --backend BACKEND` | Canonical cell path exists but is not surfaced cleanly through validate |
| Manifest convenience CLI | `manifest-cli.rs list/get` | `manifest-cli.rs run` | Duplicates parsing and command construction; not the canonical receipt path |
| Exact receipt path | `validate --requalify-cell TEST MODE BACKEND` | same | Exact cell works, but currently nests through pressure-test |
| Rust tests | nextest/cargo list surfaces | filters passed through node-specific commands | No uniform `validate step test-id` interface |
| Direct Hermit | N/A | `../bin/safehermit target/release/hermit ...` | Safely bounds a process, but has no manifest identity or receipt semantics |

A useful `validate list` should not create a fourth inventory. It should render
one joined plan from the existing outer planner and `ci/manifest-plan`, with at
least:

- profile and lane;
- outer node;
- test ID, mode, and backend for E2E cells;
- host requirements and the reason for any exclusion;
- required/occasional status;
- whether the row contributes to a qualifying full receipt;
- exact narrow command.

`validate list quick|portable-only|full|super` should make the denominator and
set relationships obvious. JSON output should be the source for human tables,
not separately maintained prose.

## Capability and prerequisite selection

The manifests use a closed `requires` vocabulary of 28 tokens. Current usage
counts include `linux` and `x86_64` on all 359 definitions, `userns` and
`ptrace` on 357, `cc` on 302, plus smaller tool and hardware requirements.

The central implementation is incomplete:

- Validate's `HostCapability` currently has only `cpuid-faulting` and `kvm`.
- Exactly two static DAG nodes declare one of those capabilities:
  `cpuid.faulting` and `test.cli_kvm`.
- In the manifest runner, only the `cpuid` requirement maps to the central
  probe path. `kvm`, PMU/RCB access, user namespaces, mount namespaces, and
  tool availability are handled elsewhere or treated as descriptive metadata.
- PMU/RCB decisions are split among workflow probes, the `pmu.preemption` DAG
  node, runtime behavior, and Rust test ignores.
- KVM decisions are split among the DAG, manifest requirements, device checks,
  and test-local early returns. A test-local early return can make a coarse node
  look green without proving that the intended test ran.
- Literal Rust `#[ignore]` annotations currently occur at 64 source sites in 19
  files, while `hermit-cli/tests/fail_closed_allowed_ignores.tsv` contains 257
  data rows. Those numbers
  are different concepts, but neither is derived from one machine-capability
  result.

Command examples used for this inventory:

```bash
for f in tests/e2e/manifests/*.yaml; do
  yq -r '.test[]?.requires[]?' "$f"
done | sort | uniq -c

rg -n '#\[ignore' --glob '*.rs'
rg -n 'HostCapability|cpuid-faulting|"kvm"' scripts ci/manifest-plan ci/dag
```

The right endpoint is one typed machine-capability probe, recorded with the
run, which all planners consume. It should distinguish at least PMU/RCB
availability, CPUID interception, `/dev/kvm`, user namespaces, mount
namespaces, architecture/kernel constraints, and required tools. Test
selection should then be a pure function of plan plus capabilities. Until that
exists, full-green reporting must enumerate every skipped or inapplicable row
and prove that it was expected; “node exited zero” is insufficient.

The current qualifying-receipt predicate is stricter about node skips than it
is about Rust filtering: it admits only the typed `empty-manifest-bucket` skip,
so `host-inapplicable` prevents a qualifying receipt, while
`ci-hub/validate/qualifying-receipt.json` currently sets
`gate_filtered_tests` to `false`. This does not yet establish the owner's
desired invariant that the hard-green denominator ran without hidden disabled
tests.

## Verification and backend parity

Current `verify` manifest cells construct one Hermit command with:

```text
hermit --log info run --base-env=minimal --backend BACKEND --strict
       --verify --verify-strict --verify-json REPORT -- GUEST ...
```

Hermit performs the two runs internally and sequentially, then compares exit
status, stdout, stderr, and canonical INFO observations. The harness consumes
the typed report. With retained logs enabled, the ptrace first-run log can be
normalized with `hermit log-diff`, but ordinary cells do not reuse that result
as a shared ptrace golden for other backends.

`tests/backend-parity/run_matrix.py` is a separate parity path. It runs a
ptrace reference and backend candidates, but its general comparison is not the
same as every candidate consuming one canonical ptrace INFO-log result.

The owner's desired external architecture is reasonable after green:

1. `manifest-cli` owns the two isolated executions for a determinism cell.
2. It may run them concurrently when resource and filesystem isolation allow.
3. It records both outputs and uses the same canonical comparator for
   determinism and backend parity.
4. A ptrace result can become an explicitly keyed golden consumed by DBT/KVM
   parity cells.

Do not replace `--verify` in the green sprint. The replacement must preserve
fresh initial state, stdin semantics, timeouts, typed no-result behavior, log
policy, and binding of a golden to exact Hermit/source/guest/environment
identity. A faster implementation that weakens any of those is not equivalent.

## Parallelism and resource accounting

The outer scheduler defaults to roughly one job per eight available CPUs,
clamped to 2–16. DAG nodes all declare wall-time and memory expectations, but
none declares an explicit CPU count. Portable nodes include 12 preferred inner
job hints and 29 scarce-resource declarations. Lane resource caps include
`hermit_guest: 1`, `manifest_guest: 8`, and privileged `kvm: 1`.

Most manifest buckets default to one inner worker. Two larger buckets use eight
workers and consume the eight `manifest_guest` slots. Some commands also embed
their own Cargo job counts without a matching outer hint.

These values are mostly maintained as estimates rather than derived from
measurements, and nested dagrun bypasses global cap accounting. Before tuning
parallelism, record per-node peak RSS, CPU use, runnable time, wait time, inner
worker count, and critical-path contribution from real full runs. Then change
one resource rule at a time and compare wall time and stability.

## Why validate is large

Measured command:

```bash
wc -l scripts/validate.rs scripts/lib/validate_*.rs
rg '^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+' \
  scripts/validate.rs scripts/lib/validate_*.rs | wc -l
```

Result: 23,435 physical lines and 433 functions. The main file alone is 16,555
lines. It includes 28 functions whose names contain `bracket`, a visible
remainder of the shell-script-style bracket output and control structure.

The size comes from owning too many concerns: CLI parsing, profile planning,
git/worktree policy, pinned-root setup, DAG rewriting, scheduler invocation,
retry aggregation, ledgers, receipts, publication, GitHub labeling, remote
integration, requalification, help text, and a large embedded self-test suite.

The surrounding duplicate surfaces add more code:

| File | Lines |
| --- | ---: |
| `ci/manifest-plan/src/runner.rs` | 5,345 |
| `ci/manifest-plan/src/bin/test-harness.rs` | 1,869 |
| `tests/manifest-cli.rs` | 1,002 |
| `scripts/manifest-to-commands.rs` | 901 |
| `ci/compat-envelope/pressure-test.rs` | 7,252 |
| `tests/backend-parity/run_matrix.py` | 1,996 |

The best path to shorter code is deletion after ownership is clarified:

- validate owns profile selection, outer-plan composition, and receipt policy;
- dagrun owns scheduling, resources, retries, and execution accounting;
- `ci/manifest-plan` owns manifest parsing, selection, and command generation;
- one test-harness CLI owns E2E execution and typed cell results;
- `safehermit` is the direct-process safety wrapper, not another test planner;
- ci-hub owns publication and repository-wide status.

That lets the duplicate manifest command builders and nested wrappers be
removed rather than rearranged.

## Staged plan

### Now: reach hard green

1. Keep `main` at the current focused baseline long enough to measure it.
2. Fix the active `build.runtime_release_in_pinned_root` cache-closure failure:
   the isolated LiteInst runtime currently requires uncached
   `foreign-types-macros` 0.2.4 under enforced `--offline`.
3. Prove that node, then continue the focused phase and recheck any of the prior
   five nodes that the plan reaches. Fix only residual measured failures. Do
   not begin the nesting, listing, or verify redesign in this loop.
4. When the focused plan is green, freeze `main` and run one `full` validate
   through ci-hub. The success condition is a qualifying receipt at HEAD and
   `commits-since-green` equal to zero.

### Immediately after zero: make the denominator visible

1. Add `validate list quick|portable-only|full|super --format text|json` by
   joining the existing outer plan with `test-harness plan`; do not create a
   new inventory.
2. Make the full receipt record expected, run, passed, failed, skipped,
   filtered, and inapplicable counts, with reasons, and refuse unexplained
   denominator loss.
3. Surface an exact narrow command for every outer node, Rust test ID where
   available, and E2E cell.
4. Add a regression check for the profile set relationships documented above.

### Then: flatten and simplify

1. Expand strict compatibility and requalification into the outer graph;
   remove production nested dagrun calls.
2. After callers are flat, make dagrun reject nesting by default. Retain an
   explicit override only for tests that intentionally exercise nesting.
3. Move manifest selection and command generation to the producer-owned
   `ci/manifest-plan` API; delete the duplicate builders.
4. Move publication-only concerns out of validate into ci-hub and shrink the
   embedded integration-style self-tests into ordinary focused test targets.
5. Introduce one typed machine-capability result and make all selection consume
   it. Then revisit ignored and disabled tests without silently weakening the
   hard-green denominator.
6. Design the external two-run/log-diff path and ptrace-golden backend parity
   on top of the stable typed cell result contract.
7. Tune scheduler resources from recorded measurements, not estimates.

## Recommendation to the landing agent

Do now: repair the offline LiteInst cache closure exposed by
`build.runtime_release_in_pinned_root` at
`330f01f88eae8264ebbc89bba34b80663f9df39a`, rerun that focused phase, repair
only what still fails, hold `main`, and launch one full ci-hub validate.

Wait until `commits-since-green=0`: `validate list`, strict-compat flattening,
dagrun nesting refusal, unified capability probing, duplicate CLI deletion,
external parallel verification/log-diff, and scheduler tuning. Those are real
improvements, but putting them in the current proof loop increases the number
of moving parts before a trustworthy baseline exists.
