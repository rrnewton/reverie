# CI signal: delivery state on 2026-09-05

## What the signal is supposed to mean

The owner's bar is direct: know what tests exist, know which tests ran, and see what they returned. A useful CI result must therefore preserve four separate facts:

1. the fixed population and the selected work;
2. what the test frameworks actually executed;
3. the exact terminal outcomes, including why a result could not be obtained; and
4. the commit, tree, configuration, run, and retained artifacts that make the result reproducible.

Missing, unreadable, contradictory, or unauthenticated input must remain unavailable or `no_result`. It must not become a pass, a product failure, zero work, or a smaller denominator. A Green scorecard cell means that full validation selects the cell; it does not by itself mean that a current run passed, and a same-backend `--verify` pass does not establish cross-backend parity.

That distinction is why the final backend account must report the integer number of fully passing cells separately from partial progress. The symbols `P`, `F`, and `D` below are proposed notation introduced in this session, not fields or terminology in the live schemas. If a backend has 200 fully passing cells and 55.5 cells' worth of partial progress, the proposed report would say `P = 200`, `F = 55.5`, and `(P + F) / D = 255.5 / D`; it must not call that 255.5 passes.

## Snapshot and limits of this report

The current parent-pinned composition checked on 2026-09-05 is:

| Role | Exact SHA | How it is selected |
| --- | --- | --- |
| `rrnewton/dev-hermit` | `f01d0ebd18e509acbcea0b81315e7e103984193c` | Parent `main` |
| `rrnewton/hermit` | `372f2e1abfe1eacebdf509efa964a26fd9d4b746` | Parent `hermit` gitlink |
| `rrnewton/reverie` | `4b18ecf0a865e2b895e7889d3bf8939a4ac6223e` | Parent `reverie` gitlink and the Reverie revision pinned by Hermit |
| `rrnewton/agent-utils` | `ed62ca84de3aae7cb249d05fe1d69e10a662d951` | Hermit `agent-utils` gitlink; the parent path resolves through Hermit |

This table describes one composed checkout; it does not combine four independent remote tips into a supposed runnable set. There is no complete full-profile validation receipt for this exact composition, so its current-tip status is **NOT-VALIDATED**. The most recent retained hard-green evidence cited below is historical evidence at older exact SHAs.

The TaskGraph vision subtree is 14/21 complete (66%). That is task bookkeeping, not backend coverage and not a product pass rate.

## Current counts on Hermit `main`

At Hermit `372f2e1a…`, the generated scorecard contains 5,744 cells:

| Status | Count | Share of all 5,744 cells |
| --- | ---: | ---: |
| Green | 677 | 11.79% |
| Red | 182 | 3.17% |
| Not applicable | 4,885 | 85.05% |

There are 859 applicable cells, of which 677 are Green: 78.81%. This second percentage is not a progress number by itself; it changes when applicability changes. Ordinary full validation selects 680 regression cells: 677 Green compatibility cells and 3 explicit custom commands.

The current verify rows are:

| Backend | Green verify cells total | Manifest verify coordinates |
| --- | ---: | ---: |
| ptrace | 340 | 359 |
| KVM | 192 | 359 |
| SaBRe | 111 | 359 |
| LiteInst | 28 | 359 |
| DBT | 0 | 359 |

These are same-backend selection counts. The checked-in scorecard explicitly reports **no cross-backend parity number**. Current Hermit `main` has 192 Green KVM verify cells in total, 191 of which overlap the 340 Green ptrace verify cells. A separate unlanded 40-cell qualification has 36 additional Green KVM cells; applying that unlanded result would produce proposed counts of 227/340 for the overlap and 228 Green KVM verify cells in total. Those proposed counts are working-lane information, not a result on live Hermit `main` and not cross-backend parity.

The current scorecard has 0 Green/`never-measured` cells. Across all 5,744 cells it records 4,944 `never-measured`, 756 `measured-and-passed`, 6 `diverged-unlocated`, and 38 `diverged`. Of the 182 Red cells, 91 are `measured-and-passed`, 60 are `never-measured`, and 31 are `diverged`. These retained observations are not automatically fresh for the current Detcore tree.

The session began from materially weaker reporting: `TESTS-PASSED` was shown as `>=1643` against `TESTS-TOTAL 2679`; 304 Green cells appeared `never-measured` despite retained canonical comparisons; and, among 371 Red cells discussed in the task, the listed batch outcomes were 221 pass, 95 infrastructure error, 13 crash, and 7 genuine divergences. Those named classes account for 336/371; the source note did not classify the remaining 35, so this report does not guess.

## Producer through scorecard

| Stage | What it should preserve | Live state |
| --- | --- | --- |
| Manifest and plan | Complete typed population, applicability, selected cells, and exact cell identity | Present. The scorecard and expected plan account for selected compatibility cells and the 3 custom commands separately. |
| Test harness | Per-test and per-cell terminal results, exact framework counts, bounded attempts, and typed reasons when no result exists | Framework-owned cell results and exact ledger counts are present. Individual manifest cells may use at most two attempts. The outer DAG retry was removed. |
| Validation service result | The exact executed and passed test counts without reconstructing one from the other | **Gap on main.** Schema 3 carries `executed_tests` but not `passed_tests`. Hermit PR #2956 adds schema 4; it is unlanded. |
| Ledger | One canonical row bound to run and source identity, with exact selected/executed/passed/filtered counts and node outcomes | Implemented on main. The authoritative count path uses typed framework outcomes and does not derive a missing passed count. |
| Receipt and status | Select the exact ledger row; reject contradictory Green counts; render exact count or unknown | Implemented on main. A passing row with unequal passed and executed counts is refused. History preserves exact-SHA/tree/no-match/unverifiable distinctions. |
| Scorecard and series | Keep selection separate from observations; retain current per-cell outcome and provenance | Same-backend status and retained observations are present. Cross-backend comparison evidence, a fixed ptrace reference population, and partial progress are absent from live schemas. |

The important remaining count gap is therefore narrower than the original failure: the ledger, receipt, and status path can report exact `passed_tests`, but the separately typed validation service result still drops it. That omission matters because parent run handles and service consumers should not have to fall back to logs or infer a count from an overall verdict.

## Why comparison and retained logs became part of this work

The owner asked for progress inside cells that have not fully passed. The first divergence supplies only the numerator: how many selected canonical INFO records matched before the difference. The fraction also needs the selected INFO total from the designated ptrace reference.

The chosen unit is **selected canonical INFO records**, using the existing `BitwiseInfoV1` policy. In the proposed notation developed in this session, the score is defined as follows:

- `D`: the fixed, versioned population of ptrace-Green reference `CellId`s that map to an applicable coordinate for the candidate backend. Candidate selection, current candidate status, and missing candidate evidence cannot shrink it.
- `P`: cells in `D` whose candidate same-backend comparison is usable and whose candidate run-1 selected INFO log matches the designated ptrace reference. A same-backend Green label alone never increments `P`.
- For each nonpassing parity cell with trustworthy evidence, contribution `c_i = selected_equal_info_prefix_i / ptrace_selected_info_total_i`.
- `F = sum(c_i)` over those nonpassing cells.
- Report `P`, `F`, `(P + F) / D`, its percentage, and the `no_result` count by exact reason.

A strict-prefix divergence can contribute 1.0 to `F` when all ptrace selected INFO records matched, but it does not increment `P`. Missing, unreadable, hash-invalid, wrong-role, wrong-cell, incompatible, empty, or out-of-bounds evidence stays `no_result` inside `D`.

This could not be added safely to the existing first-divergence field. The live stress-series schema carries a divergence coordinate and two-run runtime but does not retain the selected equal INFO prefix, the ptrace selected INFO total, or an authenticated ptrace log identity. The old cross-backend script also copied `TRUNCATION_MARKER` from `detcore::logdiff`, so the producer and consumer could silently disagree about truncated input. Loading the shared Rust comparison code removes that duplication and makes truncation refusal consistent.

The private scorecard fixture demonstrates the arithmetic only: `P = 1`, `F = 1.25`, and `(P + F) / D = 2.25 / 5 = 45%`, with two named `no_result` cells. It is uncommitted test evidence, not a backend measurement.

## Current and intended comparison paths

| Concern | Live `main` | Intended two-run path; not landed |
| --- | --- | --- |
| Who runs twice | `hermit --verify` owns the repeat internally; the harness launches one verify command | The harness launches two ordinary backend runs and owns their paths and lifecycle |
| Comparison | CLI/report path and the standalone cross-backend script do not yet share one complete retained-evidence path | The harness calls the same `detcore::logdiff` `BitwiseInfoV1` library code used by `hermit log-diff` |
| Comparable logs | Backend support is uneven; ordinary DBT log publication is only in PR #2958 | Each supported backend publishes an ordinary canonical INFO log or refuses |
| Retention | No live schema binds the log needed for later ptrace comparison | Schema-5-capable readers use an authenticated retained run-1 gzip descriptor bound to role, `CellId`, attempt, sizes, and SHA-256; the active writer must remain schema 4 until the cutover is complete |
| Reference | No designated ptrace artifact supplies a durable per-cell total | A qualified schema-10 history row, retained-log index, exact repository/tree/profile/run identity, and image digest identify the ptrace reference under one state root |
| Score | Green/Red and observation states only | Stress series v4 carries comparison evidence; pressure test compares candidate bytes with the designated ptrace bytes; scorecard reports `P`, `F`, `(P+F)/D`, percentage, and `no_result` reasons incrementally by `CellId` |

## Delivered on `main`

These are delivered behavior, not merely findings or tests:

| Change | Landed anchor | Human-visible before and after |
| --- | --- | --- |
| Unavailable inputs stay indeterminate | dev-hermit `b30574084d7d958236c64d93aeb8d2fbe9a00622` | Seventeen decision surfaces previously allowed missing or malformed populations, baselines, provenance, Git data, or TaskGraph rows to resemble an empty/clean answer. They now return an explicit indeterminate/unavailable result. |
| Optional run numbers and malformed-record isolation | dev-hermit `fc5931dfbea84cffeb9723c36b842195a162682a` and `3b88d2edbe1f9677c1b4075f41d41248908884c0` | A terminal historical handle may lack a display run number without blocking live status, while an unreadable handle is reported separately and cannot hide valid runs or become absence. |
| Landing validation handoff preserves refusal | dev-hermit `5c8cb4adce402dbcb0b15fa2d6fa7a4bf1411643` | A real `COULD_NOT_RUN` refusal remains a refusal through `pr_head_ci`/landing handling instead of becoming a product failure; checkout mutation ownership is also fenced. |
| Outer DAG retries removed | Hermit `42d5550e3e5f858b2b2d6e46644742a6269509e9` | Validation launches the DAG once and records `retry_rounds=0`; only the test harness retains its bounded two-attempt cell retry. This prevents four 900-second attempts from consuming a 3,600-second outer deadline. |
| Native run timeout and typed timeout reporting | Hermit `91d93b97…` through `04c11004cf41283507e730bcefeddd03b46fd171`, clarified at `62ae2aeab9f1f354466add0045d6851a5aeda236` | `hermit run --timeout` bounds supported runs, names the timeout in stderr, and refuses backends where it cannot provide the promised bound instead of silently accepting the flag. |
| Per-cell CPU and wall bounds | Hermit `976c70986b3c4a301295d1e04351bff48e22fbe1` | Cells now record and enforce independent process-group CPU and wall limits, with independent machine multipliers and typed `cpu-timeout`/`wall-timeout` outcomes. |
| Trustworthy concurrency registration | Hermit `6976687a0b2d0f7660324e8ef787bd882b9627b5` | Registry create/open/lock/write or identity failures refuse before the DAG instead of silently reporting zero peers. |
| Proc-lock test isolation | Hermit `efdf11747c8a41a4cf65182a5918ead361d9cf98` | The known width-three host OFD-lock interference in that test is isolated without weakening the strict verification assertion. |
| Measured intra-step work | Hermit `aa37ce9625989c3bff38362549d0699ee4fe9eea` | Manifest guest preparation uses measured width 16; independent manifest audits use width 2; rust-script packages build through one Cargo workspace. |
| DBT diagnostic/initialization separation | Reverie PR #523, current main `4b18ecf0a865e2b895e7889d3bf8939a4ac6223e` | DBT can separate its own diagnostics and initialization evidence. This is a prerequisite, not the Hermit harness cutover. |

The authoritative ledger/receipt/status path for exact framework pass counts is also present on the current main pair. Its retained proof is RUN1703 (`validate-codex-pin-validate-084cf4a336f6-1788502981813981138-1010177-7a55f6fc`) at Hermit `084cf4a3…`: 267/267 nodes and 2,564/2,564 tests, with zero failures, skips, retries, or timeouts. That run is historical; it is not evidence for the current composition listed above.

## Implemented or prepared, but not landed

| Work | Live state | What it changes |
| --- | --- | --- |
| Exact `passed_tests` in the validation service result | Hermit PR #2956, OPEN draft, head `45c98f92104707d0cd5a949aa0be919a7b44db2d`, base `89e684c8…` | Adds schema 4, copies the exact framework count, rejects negative or impossible counts, and requires passed=executed for `PASSED`; schemas 1-3 remain exact historical readers. Focused tests are green, but this is not delivered behavior. |
| Parent consumption of that count | dev-hermit PR #283, OPEN draft, head `85dcde520d7b85fe4fb09995d81bcc07aa54afe2`, base `b1359090…` | Preserves the exact count through service parsing, run registry, pane observer, cross-check, and release acceptance; historical absence remains unknown. It depends on #2956 and is not delivered. |
| GitHub check authority outage classification | Hermit PR #2953, OPEN, head `6d6e76a94d0d6e662d3b27189a60d5db63931724`, base `89e684c8…` | Treats an HTTP 504 or other identified transport failure as `COULD-NOT-DETERMINE`/no signal, while 403/404, digest mismatch, and other refusals remain loud. It also obtains the pinned authority once so a later fetch cannot change the node's answer. Not landed. |
| Harness-managed verify and retained-log preparation | Hermit PR #2958, OPEN draft, current head `f18b76cc464a3f8f7704f2d7abdbf53baff157e0`, parent `88af42318cda6d554e8235b05a2edead51c30e3d`, tree `e1b14b21505a24567f36d3d430923c958bd1e681`, base `500dded1…` | Contains shared comparison work, ordinary DBT log publication, typed DBT evidence, retained-log preparation, authenticated retained-log/index APIs, and the independently reviewed shared selected-equal-INFO-prefix result. The live harness still uses the existing `--verify-json` path; the PR is a scaffold, not the cutover, and is not on `main`. |
| Partial-credit score | private managed branch `codex/partial-credit-score` based on `88af42318…`; uncommitted scorecard-only diff, patch-id `8428e868dd04a4165b9b2df59d64bbec70cee5a0` | Implements the producer-independent fixed denominator, replacement/idempotence rules, arithmetic, explicit `no_result` accounting, and rendering fixtures. No public schema, commit, PR, or backend score exists. |
| SaBRe protected log descriptor work | Reverie PR #522, OPEN, head `276b97f7ff0e0a0fdd959693897cd2b2da4d7d8d`, base `320412c5…` | Attempts to protect the inherited SaBRe sink descriptor, but `sendfile` is absent from the protected-fd syscall guard, so a guest can still access a protected descriptor through that syscall. It is not safe to call this an authoritative ordinary log channel. |

All three draft count/verify PR bases precede their repositories' current `main`, so they require refresh and exact-head revalidation. None of their focused tests can be used to call current `main` green.

## Remaining blockers and their causes

1. **The validation service result still loses the exact passed count.** The framework has the count and the ledger retains it, but the mainline schema-3 sidecar has no field. Until #2956 and #283 land together, consumers of that sidecar can only report unknown or consult another source.

2. **A GitHub transport outage can still make a validation node look red.** PR #2953 contains the classification and single-fetch fix, but current main does not. The observed failure was an HTTP 504 after an earlier successful probe; without the fix, the checker exited nonzero and the DAG recorded a failed gate even though no check verdict had been obtained.

3. **The two-run change has not crossed the live writer boundary.** The retained-log reader/types can be added compatibly, but enabling CellResult schema 5 before every split-run path publishes descriptors would create rows that claim evidence they do not carry. The active writer must remain schema 4 until run production, retention, validation, and all consumers change together.

4. **The parent does not yet publish and verify the retained-log artifact required by the score.** A candidate log cannot be compared to an arbitrary historical file. The designated ptrace reference must be selected from one qualified schema-10 history row and authenticated index under the same state root, with run, commit/tree/profile, population, image digest, cell, attempt, role, size, and hashes checked. Missing or incompatible reference data must be `no_result` and cannot reduce `D`.

5. **`NoResult` still loses its cause at the status boundary.** Current `validate_status.rs` maps multiple producer and legacy conditions to one `NoResult` disposition. That is safer than calling them failures, but it cannot yet explain whether the cause was not run, first-run rejection, timeout, OOM, infrastructure failure, noncanonical comparison, or missing evidence. The scorecard must preserve an existing bounded typed reason; it must not copy raw stderr or invent a cause.

6. **Backend log channels are incomplete.** Reverie #523 delivered DBT diagnostic separation, but Hermit's ordinary DBT log publication remains in unlanded #2958, and DBT still shares core and guest file descriptors 1/2, so exact stdout/stderr cannot yet be claimed. SaBRe #522 remains unsafe because `sendfile` is not guarded when either of its descriptor arguments names the protected sink. Unsupported or ambiguous capture must refuse.

7. **No real backend partial-credit result can be computed yet.** Current rows do not carry the numerator, ptrace total, or authenticated reference identity. Current main has 192 Green KVM verify cells total and 191/340 overlap with the ptrace-Green set; the unlanded 36/40 qualification would propose 228 total and 227/340 overlap. Neither set of same-backend counts is a substitute for the proposed `P`, `F`, or `(P+F)/D` comparison score.

8. **There is no current-tip full validation.** RUN1703 (`validate-codex-pin-validate-084cf4a336f6-1788502981813981138-1010177-7a55f6fc`) proves an older exact tree; the repositories have advanced. The current parent-pinned composition remains **NOT-VALIDATED**. A fresh qualifying run must follow the landed dependency stack; until then, “current main is green” would be unsupported.

9. **The clean concurrency claim stops at width 2.** A width-three run exposed one real shared-test defect; its isolation fix landed, but no post-fix three-full-validate run has demonstrated width 3. Six slots describe admission capacity, not measured clean width.

10. **Intra-step work is partial.** Manifest guest preparation and rust-script preparation improved, but backend-parity-c at width 32 under concurrent full validation and `hermit_unit` above `-j1` have not been established safe. The literal all-step width study did not produce a complete delivery claim.

## Concurrency, timeout, and intra-step evidence

The strongest clean concurrency evidence is at Hermit `a58e9ecfb037f4811330b2383de7806b404c25e2` on one host and boot:

- RUN1688 (`validate-mega-lander-a58e9ecfb037-1788466018827714360-1307714-1d639793`) solo: 267/267 nodes, 2,557/2,557 tests, 496 filtered, zero failures/skips/retries/timeouts; wall 1,198 s; CPU 4,649.038 s.
- RUN1689 (`validate-mega-lander-concurrency-a58-a-a58e9ecfb037-1788471074704903712-309759-9b63c25d`) and RUN1690 (`validate-mega-lander-concurrency-a58-b-a58e9ecfb037-1788471077633460167-311333-cf54cc4d`): each 267/267 nodes and 2,557/2,557 tests, zero failures/skips/retries/timeouts; starts 4 s apart; 1,396/1,396 s overlap of the shorter arm.
- Pair window: 1,443 s. Measured serial reference: `2 × 1,198 = 2,396 s`. Saving: `1 - 1,443 / 2,396 = 39.77%`.
- Arm CPU: 5,054.625 s and 5,066.413 s. Aggregate overhead: `(5,054.625 + 5,066.413) / (2 × 4,649.038) - 1 = 8.85%`.

This establishes one clean width-two condition. The outer parent tools differed across the solo and pair, although the retained child envelope, Hermit commit/tree, host/boot/toolchain, plan, test denominator, and cell identities matched. It is not a population estimate.

The width-three experiment at Hermit `a58e9ecf…` produced RUN1691 (`validate-concurrency-a58-width3-c-20260903T221205Z`) and RUN1692 (`validate-concurrency-a58-width3-b-20260903T221205Z`) at 267/267 nodes and 2,557/2,557 tests, while RUN1693 (`validate-concurrency-a58-width3-a-20260903T221205Z`) failed 1/2,557 tests in `proc_locks_determinism`. The focused reproducer made all 3/3 simultaneous old test instances fail. The isolation repair landed at `efdf1174…`, but the full width-three experiment has not been repeated.

Timeout policy is now measured and typed. The per-cell calibration used 26,702 unique PASS rows over all 492 then-selected CI cells. Defaults are CPU 22 s and wall 57 s, with measured overrides. Four audited historical hard-green runs—RUN1688 (`validate-mega-lander-a58e9ecfb037-1788466018827714360-1307714-1d639793`), RUN1689 (`validate-mega-lander-concurrency-a58-a-a58e9ecfb037-1788471074704903712-309759-9b63c25d`), RUN1690 (`validate-mega-lander-concurrency-a58-b-a58e9ecfb037-1788471077633460167-311333-cf54cc4d`), and RUN1703 (`validate-codex-pin-validate-084cf4a336f6-1788502981813981138-1010177-7a55f6fc`)—recorded no timed-out nodes, retries, or unreported nodes. This is evidence for those runs, not all healthy runs and not current tip.

The failure that motivated outer-retry removal shows why the layers matter. RUN1572 (`validate-root_unselected_cells_next-e12c472d22d2-1788104407454379762-2184092-4423268f`) executed 60 nodes, dependency-skipped 16, and recorded 1,910/1,910 tests passed, yet validation was incomplete after `gate.manifest` exceeded 300 CPU seconds at 422.2 s wall. A generic outer retry then spent another 260.5 s without repairing the missing dependent work. The current design runs the DAG once and leaves retry ownership with the test harness.

Delivered intra-step measurements are narrower but real:

- Manifest `c-programs` preparation at width 1 measured 10.04/9.64/9.75 s wall; width 16 measured 1.29/1.29/1.29 s. Median wall speedup is `9.75 / 1.29 = 7.56×`; median CPU was essentially unchanged, 8.17 s versus 8.15 s.
- Rust-script preparation moved from 212.34 s wall / 688.10 s CPU to 72.03 s wall / 427.19 s CPU, while publishing the same 26 entrypoints and passing the same 15 harnesses.
- Backend-parity-c measured 13.9 s at width 8 and 7.47 s at width 32, a 1.86× wall improvement, but width 32 was not tested inside concurrent full validation and is not the production setting.

## Dependency-ordered recovery plan

1. Independently review, rebase, and land Hermit #2956 at current Hermit `main`; rerun its schema, validate self-test, formatting, and focused Clippy gates at the new exact head.
2. Update dev-hermit #283 to the actual landed Hermit SHA, rebase onto current dev-hermit `main`, rerun the service-result/run-registry/cross-check/acceptance tests, then land it. Verify that an exact failed mixed run preserves a lower exact passed count and a passing run requires passed=executed.
3. Land or otherwise resolve Hermit #2953 before relying on GitHub status during final validation, so an unavailable authority is no signal and an actual refusal still fails loudly.
4. Refresh #2958, whose current head is `f18b76cc…`, onto current Hermit `main`. First land only the backward-compatible shared comparator, authenticated retained-log/index APIs, selected-prefix result, and schema-5-capable readers; keep the active writer at schema 4.
5. Finish backend-specific prerequisites before activation. Replace SaBRe's unsafe inherited-descriptor design with a genuinely isolated producer; patching only the observed `sendfile` cases would leave the same class of hole in other descriptor-bearing operations. Separate DBT guest stdout/stderr from DynamoRIO core diagnostics. Until those boundaries exist, the affected capture paths must refuse rather than make a reduced exact-stream claim.
6. Add the matching dev-hermit schema-10 retained-artifact publication, verification, and readers. Prove index and gzip integrity, fixed ptrace population identity, image digest, run/cell/attempt/role binding, restart cleanup, and refusal on missing or malformed evidence.
7. Land the downstream pressure-test, stress-series v4, scorecard, and generated-output readers. Preserve v1-v3 readers, replace a cell's contribution only with a newer event, make duplicate replay byte-idempotent, and render the proposed `P`, `F`, `(P+F)/D`, percentage, population identity, and exact `no_result` reasons on every incremental update.
8. Only after the backend prerequisites and every schema-5/schema-10 reader are present, activate the split-run producer, retention, validation, and schema-5 writer together. Every live split-run path must emit the authenticated descriptor; unsupported or incomplete paths must refuse.
9. Run one fresh full-profile validation at the final exact parent-pinned Hermit/Reverie/agent-utils composition. Require complete selected node and test counts, no unexplained skipped/unreported work, and retain the exact receipt. Only then make a current-tip green claim.
10. Repeat the full width-three validation after the landed proc-lock isolation fix. Keep width 2 as the maximum demonstrated clean width until all three arms are complete and clean.
11. Have an independent reviewer compare the landed files and fresh receipt against every vision item. Close the vision only when the review confirms the behavior is on `main`; focused branch tests and private fixtures remain labeled unlanded until then.

## Bottom line

The project can now distinguish much more of “what ran” from “what was selected,” and the ledger/receipt/status path can carry exact framework counts. It cannot yet give the owner's requested full backend account. The service-result count pair, outage classification, two-run retained-log path, designated ptrace reference, and partial-credit consumers remain unlanded or unfinished, and no full validation covers the current parent-pinned composition. Its current-tip status remains **NOT-VALIDATED**. The safe current statement is therefore: exact count reporting is substantially delivered, comparison retention and partial progress are prepared, but the proposed cross-backend `P`, `F`, and `(P+F)/D` values are not yet measurable.
