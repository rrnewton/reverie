# Functional prose readers and shared JSON contracts

Date: 2026-08-29

## Result

At the audited remote heads, **47 current producer/consumer paths in 46 code
sites** still obtain a functional value from text rather than consuming JSON
backed by one shared Rust type.

| Current disposition | Producer/consumer paths | Code sites |
|---|---:|---:|
| A published change exists, but is not on every required main branch | 20 | 19 |
| No published replacement exists | 21 | 21 |
| The parser is the innermost available adapter for an external or still-running process | 6 | 6 |
| **Total current paths** | **47** | **46** |

The complete parser inventory also records paths that cannot silently decide a
current result:

| Authority | Producer/consumer paths | Code sites |
|---|---:|---:|
| Current functional authority | 47 | 46 |
| Historical-only fallback | 5 | 5 |
| Non-authoritative consistency or diagnostic reader | 10 | 6 |
| Dormant, with no production caller | 3 | 3 |
| **All enumerated paths** | **65** | **60** |

The path/site difference is deliberate: Reverie's `validate.sh` independently
derives per-gate and whole-run counts in one file. A site is one place that
makes a semantic decision, not one regular-expression literal.

This is a census, not a claim that every published change satisfies the final
directive. In particular, JSON parsed through a second local struct or through
`serde_json::Value` does **not** meet the one-shared-Rust-type requirement.
Exact line protocols are safer than arbitrary prose but are still not JSON.

The audit was performed by reading production and maintained harness code at:

- dev-hermit `a524bf4d9c2ade8aa9990e3c9ec695a1573a8633`
- Hermit `20b265fa3eb580360ff93304d4790f5664a3705a`
- Reverie `bc106a195dfe7ba8e00c358a7bab11256202a1e6`
- LiteInst2 `ac6df266c76dcd5c724df958321ff4f9c0b23f63`
- agent-utils `0cdc88673e484530439c64047ba2e78c538cba5b`

The search started from the earlier 391-candidate/150-file audit, re-read the
two published follow-ups, compared every changed production file since their
pinned heads, and then searched all five current trees again. No production
functional prose reader was found in LiteInst2. Parent source citations remain
pinned to `89d8d28e`; later commits through the audited `a524bf4d` head were
read separately and add no new functional prose path. Hermit citations remain
pinned to `6c2f025c`; the later scorecard write-back change through the audited
`20b265fa` head was also read and adds no new functional prose path.

All eight backlog tasks named by the directive were re-read. Their implemented
paths appear either in the published-change table or the controls below; their
remaining paths appear in the no-replacement or correctly-outer tables.

Every row in the three current-path tables below was established by reading
the cited producer and every production consumer. Evidence established by
running is called out separately in **Exclusions and controls**; the census did
not turn a successful command into evidence for a source path it had not read.

## Published changes not yet present on every required main branch

These paths remain current until their listed changes land. `Shared type` says
whether the published implementation already reaches the owner's final rule.

| # | Functional value and current consumer | Producer | Published disposition | Shared type |
|---:|---|---|---|---|
| 1 | Validation exit, terminal status, and executed node/test counts in parent [`service_result.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/validate/service_result.py#L55), called by `start_unit.py` and `pane_watch.py` | Hermit's validation framework already owns the complete terminal result | [Hermit PR 2811](https://github.com/rrnewton/hermit/pull/2811) writes `ValidationServiceResult`; [dev-hermit PR 220](https://github.com/rrnewton/dev-hermit/pull/220) consumes it | No across the Python boundary: the producer type and checked schema agree, but Python repeats the fields |
| 2 | DBT parity case identity, attempt, and verdict in Hermit [`dbt_parity_test_observations`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/validate.rs#L14637) | `run_matrix.py` already owns every case result | [Hermit PR 2832](https://github.com/rrnewton/hermit/pull/2832) writes `dagrun::TestResults` | Yes |
| 3-4 | Reverie per-gate and whole-run test counts in [`validate.sh`](https://github.com/rrnewton/reverie/blob/bc106a195dfe7ba8e00c358a7bab11256202a1e6/validate.sh#L159) | Cargo/libtest | [Reverie PR 513](https://github.com/rrnewton/reverie/pull/513), stacked [PR 514](https://github.com/rrnewton/reverie/pull/514), and [dev-hermit PR 220](https://github.com/rrnewton/dev-hermit/pull/220) | No: Reverie defines a local `TestCounts`; Python repeats its fields |
| 5 | Release validation identity, selection, status, and denominator in [`manual_validate`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/scripts/release-0.3-acceptance.py#L318) | Hermit validation service result | [dev-hermit PR 220](https://github.com/rrnewton/dev-hermit/pull/220) | No: the Python reader validates a separately maintained shape |
| 6 | CPUID prerequisite in [`run_matrix.py`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/tests/backend-parity/run_matrix.py#L932) | Hermit host-capability probe | [Hermit PR 2828](https://github.com/rrnewton/hermit/pull/2828) | No at the Python boundary: Python repeats the Rust record's keys |
| 7 | Tested binary build SHA in [`probe_binary_build_sha`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/ci/manifest-plan/src/runner.rs#L3189) | Hermit build information | [Hermit PR 2828](https://github.com/rrnewton/hermit/pull/2828) | Yes inside Rust |
| 8 | Compile-time e9patch availability in [`prerequisites`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/tests/backend-parity/e9patch_corpus.py#L204) | Hermit build information | [Hermit PR 2828](https://github.com/rrnewton/hermit/pull/2828) | No at the Python boundary |
| 9 | DBT branch/syscall summary in [`detcore_summary`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/hermit-cli/src/bin/hermit/backends.rs#L1252) | `DbtBackendStatsSnapshot` | [Hermit PR 2828](https://github.com/rrnewton/hermit/pull/2828) | Yes inside Rust |
| 10 | Backend engagement in parent [`backend_engagement`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-envelope.rs#L1474) | Hermit backend implementations | [Hermit PR 2828](https://github.com/rrnewton/hermit/pull/2828) and [dev-hermit PR 217](https://github.com/rrnewton/dev-hermit/pull/217) | No across repositories: the parent reads the JSON through `Value` |
| 11 | DETLOG event class, turn, virtual time, and syscall in [`logdiff.rs`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/detcore/src/logdiff.rs#L616) | Detcore | [Hermit PR 2831](https://github.com/rrnewton/hermit/pull/2831) | Yes: `detcore-model::DetLogRecord`; retained prose stays historical-only |
| 12 | Guest-launch refusal in manifest-plan [`runner.rs`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/ci/manifest-plan/src/runner.rs#L1822) | Hermit CLI | [Hermit PR 2829](https://github.com/rrnewton/hermit/pull/2829) | No: the published transport is an exact class line, not JSON |
| 13 | Backend unavailability in manifest-plan [`runner.rs`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/ci/manifest-plan/src/runner.rs#L1858) | Hermit CLI | [Hermit PR 2829](https://github.com/rrnewton/hermit/pull/2829) | No: the published transport is an exact class line, not JSON |
| 14 | e9tool coverage in [`e9patch_corpus.py`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/tests/backend-parity/e9patch_corpus.py#L186) | external e9tool | [Hermit PR 2829](https://github.com/rrnewton/hermit/pull/2829) adds a strict adapter | External producer; adapter cannot verify semantic truth |
| 15 | e9tool coverage in Hermit [`e9patch.rs`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/hermit-cli/src/e9patch.rs#L1095) | external e9tool | [Hermit PR 2829](https://github.com/rrnewton/hermit/pull/2829) adds the producer-owned result | External input remains an adapter; project output is typed |
| 16 | Backend availability in parent [`backend_unavailable_reason`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-envelope.rs#L1347) | Hermit CLI | [Hermit PR 2829](https://github.com/rrnewton/hermit/pull/2829) and [dev-hermit PR 218](https://github.com/rrnewton/dev-hermit/pull/218) | No: exact class text remains the cross-repository transport |
| 17 | Merge-authority state in [`merge_gate_authority_gate`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/health/operational_health.py#L1620) | merge-authority checker | [dev-hermit PR 218](https://github.com/rrnewton/dev-hermit/pull/218) adds JSON | No shared Rust type; Python owns both ends today |
| 18 | Timeout/OOM attribution in Hermit [`reason_is_budget_breach`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/validate.rs#L11894) | dagrun `StepOutcome` typed termination fields | [Hermit PR 2822](https://github.com/rrnewton/hermit/pull/2822) contains the consumer move but needs a current-main content port | Yes once ported: `dagrun::StepOutcome` |
| 19 | Timeout/OOM attribution in [`pressure-test.rs`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/ci/compat-envelope/pressure-test.rs#L980) | dagrun `StepOutcome` typed termination fields | Same [Hermit PR 2822](https://github.com/rrnewton/hermit/pull/2822); retained schema-1 prose must stay non-authoritative | Yes once ported |
| 20 | Nextest aggregate executed and filtered counts in [`run-nextest-counted.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/ci/run-nextest-counted.sh#L22) | Nextest `libtest-json-plus` events | [Hermit PR 2827](https://github.com/rrnewton/hermit/pull/2827) derives the counts from typed events and writes shared `dagrun::TestResults`; focused tests pass, while exact-head full run 1557 had a LiteInst failure outside the modified paths | Yes for the project result; nextest remains an external JSON adapter |

## Current paths with no published replacement

| # | Functional value and consumer | Producer position | Required direction |
|---:|---|---|---|
| 21 | Landing preflight executed-test count in [`preflight.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/landing/preflight.py#L109) | The validation framework already writes exact typed counts | Consume the validation result; absent evidence must refuse, not scan the log |
| 22 | Unevaluated lint-check cases in [`lint-checks-node.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/ci/lint-checks-node.sh#L46) | `test_validate_stop_paths.py` owns the list | Replace the exact `NO-RESULT-CASE:` line protocol with a typed result file |
| 23 | Union-rebase result in [`land-pr.sh`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/landing/land-pr.sh#L515) | `union-rebase.sh` owns the closed result | Emit and consume JSON backed by one Rust result type |
| 24 | Review approval/refusal authority in [`approval_binding.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/health/approval_binding.py#L676) | Humans write comments; the project owns the durable record | Preserve prose as explanation, but bind authority in a typed record |
| 25 | Cherry-pick source, disclosure, and reviewed context in [`cherry_pick_context.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/landing/cherry_pick_context.py#L175) | Git commits, PRs, and reviews | Produce one durable typed record; prose cannot remain the authority |
| 26 | Fail-closed progress count and first failure in [`progress-report.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/progress-report.sh#L89) | `test-fail-closed.sh` | Add a typed report; manual-only, so lower measurement priority |
| 27 | Record/replay counts in [`progress-report.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/progress-report.sh#L48) | Cargo/libtest | Consume a shared test-result file |
| 28 | Per-application counts in [`progress-report.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/progress-report.sh#L122) | Cargo/libtest | Consume the same shared test-result file, not a second parser in the same script |
| 29 | Optional rr-suite counts in [`compat-map.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/compat-map.sh#L257) | Cargo/libtest | Consume a shared test-result file |
| 30 | Exact and ignored test inventory in [`test-fail-closed.sh`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/test-fail-closed.sh#L110) | Cargo/libtest | Use structured listing output and a shared inventory type |
| 31 | Exact-test presence in [`stress-burst`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/stress/stress-burst#L169) | The test binary | Record the inventory rather than select `NOTEST` from display text |
| 32 | Burst pass/hang/other totals in [`stress-burst`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/stress/stress-burst#L206) | The per-instance wrapper already knows exit and timeout | Replace bare exit-code lines with one typed per-instance result |
| 33 | Demo 8 cold-calibration versus cached-seed path in [`demo-hot-path.yml`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/.github/workflows/demo-hot-path.yml#L303) | `prepare-demo08-assets.sh` | Emit a typed preparation result; presentation stays unchanged |
| 34 | Final Reverie validation status in [`reverie_safe_ci.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/validate/reverie_safe_ci.py#L23) | `reverie_node.py` | Replace the exact text token file with typed JSON; currently fail-closed but not shared Rust |
| 35 | Verification pass in [`collect-e9patch-compat.rs`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-e9patch-compat.rs#L134) | Hermit verification report already exists | Consume the shared verification-report parser |
| 36 | e9patch reach counts in [`collect-e9patch-compat.rs`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-e9patch-compat.rs#L209) | Hermit/e9patch owns the values | Consume the shared backend-engagement result after its cross-repository reader is shared |
| 37 | Box timeout in [`collect-fullcorpus.sh`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-fullcorpus.sh#L88) | `hermit-box-run` knows termination | Serialize and read its typed termination result |
| 38 | Unsupported versus failed verification in [`collect-fullcorpus.sh`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-fullcorpus.sh#L404) | Hermit owns verification and backend refusal | Consume the typed verification and process result |
| 39 | Box timeout/refusal in [`collect-dbi-corpus.rs`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-dbi-corpus.rs#L172) | `hermit-box-run` and Hermit own the two facts | Consume typed termination plus typed Hermit process result |
| 40 | Reverie syscall count in [`collect-reverie-compat.rs`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-reverie-compat.rs#L232) | Reverie tools own the count | Add one shared backend-statistics result rather than per-tool text spellings |
| 41 | Reference-guest witness in [`reference_guest.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/parity/reference_guest.py#L116) | The repository-owned guest knows whether it exercised the dimension | Serialize the witness; an absent record must refuse rather than inspect stdout |

## Parsers that are correctly outer today

These still parse output, but moving their result inward is not currently
possible without changing an external producer or losing evidence from a
process that has not terminated. They must remain closed adapters: unknown text
produces unknown/refusal, never a guessed pass or zero.

| # | Consumer | Why it remains outer |
|---:|---|---|
| 42 | Hermit [`environmental_block_observation`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/lib/validate_runtime.rs#L398) | Cargo, shell, proxy, jailer, and third-party tools do not emit the project's `FailureClass`; manifest cells already bypass this adapter with their own typed result |
| 43 | agent-utils [`captured_test_counts`](https://github.com/rrnewton/agent-utils/blob/0cdc88673e484530439c64047ba2e78c538cba5b/rs/dagrun/src/scheduler.rs#L2456) | Generic commands may expose only libtest-style output; Hermit receipt mode requires a structured count file, so this fallback cannot set its denominator |
| 44 | agent-utils Rust live [`recognize`](https://github.com/rrnewton/agent-utils/blob/0cdc88673e484530439c64047ba2e78c538cba5b/rs/dagrun/src/attribution.rs#L383) | A hung external libtest/pytest/TAP process cannot write a terminal record; unknown text yields no test identity |
| 45 | agent-utils Python live [`recognize`](https://github.com/rrnewton/agent-utils/blob/0cdc88673e484530439c64047ba2e78c538cba5b/py/dagrun/attribution.py#L275) | Same live-process boundary as the Rust reader |
| 46 | Parent stress [`classify_shape`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/attribution/attribution.py#L644) | Timeout and exit are typed; arbitrary commands do not share richer mismatch/crash/harness results, and unmatched text stays unknown |
| 47 | Hermit stress [`categorize`](https://github.com/rrnewton/hermit/blob/6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd/scripts/stress-test.sh#L120) | Per-test pass/fail comes from JUnit; the project's infrastructure category becomes true only across iterations and nextest does not own it |

## JSON that is not backed by the shared producer type

This is outside the 47 prose paths but is part of the owner's directive because
JSON alone does not prevent drift.

- Parent [`verify_tier_from_json`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/compat-envelope/collect-envelope.rs#L1040)
  reads Hermit's verification report through a local `VerifyVerdict` assembled
  from `serde_json::Value`. It should call the Rust parser that owns
  `VerificationReport`, or import that exact type.
- The published host-capability and backend-engagement Python/parent readers in
  [Hermit PR 2828](https://github.com/rrnewton/hermit/pull/2828) and
  [dev-hermit PR 217](https://github.com/rrnewton/dev-hermit/pull/217) validate
  repeated key sets rather than delegating to the producer's Rust types.
- The published Reverie count and release-result path in
  [Reverie PR 513](https://github.com/rrnewton/reverie/pull/513), stacked
  [PR 514](https://github.com/rrnewton/reverie/pull/514), and
  [dev-hermit PR 220](https://github.com/rrnewton/dev-hermit/pull/220) has a
  local Rust `TestCounts` producer and Python dictionary readers, not one shared
  Rust definition across the boundary.
- The validation-service result in
  [Hermit PR 2811](https://github.com/rrnewton/hermit/pull/2811) and
  [dev-hermit PR 220](https://github.com/rrnewton/dev-hermit/pull/220) checks a
  generated schema and exact Python field set, but the Python readers do not
  import or delegate to `ValidationServiceResult` itself.

For non-Rust consumers, maximal compile-time checking means a small Rust entry
point imports the producer's type, parses and exhaustively classifies it, and
emits the consumer-facing JSON. Missing executable, nonzero exit, empty output,
malformed response, missing field, and an unknown variant are reader failures;
none may fall back to still-present prose.

## Historical-only prose fallbacks

These five readers still interpret prose for retained records written before a
typed field or record existed. They are not included in the 47 current paths:
the older shape remains readable under its historical meaning but earns no
current authority.

| Consumer | Historical allowance |
|---|---|
| [`failure_evidence.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/validate/failure_evidence.py#L714) | Uses message signatures only for rows written before the closed `FAULT` marker or `failure_class` existed |
| [`flake_class.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/validate/flake_class.py#L303) | Recovers failing cells from retained logs when the schema-2 registry entry is unavailable |
| [`finalize_receipt.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/validate/finalize_receipt.py) | Retains the older log-derived coverage path without allowing it to satisfy current receipt authority |
| [`task_pr_join.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/landing/task_pr_join.py) | Retains the earlier task/PR text join for records predating explicit publication bindings |
| [`drain_publication.py`](https://github.com/rrnewton/dev-hermit/blob/89d8d28eb2da4bf2631aad3a672fff7d4f47df45/ci-hub/closure/drain_publication.py#L404) | Falls back to historical PR links and exemptions in task prose while reporting `source=regex` distinctly from the current attributed record |

## Non-authoritative and dormant readers

These readers were also rechecked by reading. They parse text, but their output
does not select a current verdict, denominator, or authority, so they are not
part of the 47-path remainder.

- Hermit's validation command/manifest comparison and
  `audit-test-binary-registration.py` are consistency checks beside typed
  manifest fields.
- `pressure-test.rs` compares a retained classifier with the typed cell result
  and refuses disagreement; `failure_evidence.py` retains message signatures
  as diagnostics after the typed class is selected.
- `owner-checkout-validate-watch.py` has five reporting paths for lock-holder,
  admission-refusal, privileged-node, relink-recurrence, and relink-retirement
  detail. Their recorded verdict effect is none.
- Parent `signal_crosscheck.py` parses the rendered scorecard only to expose a
  disagreement with `cells.json`; it cannot make the scorecard pass.
- The three dormant readers are `aggregate.py`, `nonzero_result.py`, and
  `sabre_reach.py`; none has a production caller.

## Exclusions and controls

Not counted as current functional prose authority:

- eleven verification-banner consumers now delegate to Hermit's shared
  verification-report reader on main; their mutation tests hold the banner
  fixed and move the typed verdict;
- the three divergence tests that formerly asserted exact stderr phrases now
  request the verification report and assert its typed verdict and fields;
- the separate local-enum audit re-counted 3,247 Rust `match` expressions and
  removed all 11 manually confirmed fail-open defaults in its published
  [Hermit](https://github.com/rrnewton/hermit/pull/2834),
  [Reverie](https://github.com/rrnewton/reverie/pull/515), and
  [parent](https://github.com/rrnewton/dev-hermit/pull/222) stack; its mutation
  probes fail with named E0004 non-exhaustive-match errors;
- Hermit's per-test nextest observations already come from typed terminal rows;
- Hermit's nextest aggregate counts have a published move to the versioned
  `libtest-json-plus` event stream and shared `dagrun::TestResults` in PR 2827;
  until it lands, row 20 remains a current prose path;
- scorecard schema and validation-step identity checks now use shared types;
- owner-watch log parsing and `failure_evidence.py` message signatures are
  presentation or diagnostics after a typed decision;
- retained schema-1/history readers remain readable but gain no current
  authority;
- dormant `aggregate.py`, `nonzero_result.py`, and `sabre_reach.py` parsers have
  no production caller;
- source-language parsing, exact SHA/path validation, and test assertions about
  human diagnostics are not tool-output-to-functional-result boundaries.

## Order

Order remains measurement relevance, not implementation effort:

1. Receipt verdicts and denominators: rows 1-5, 20-22, and 35-41.
2. Producer identity, prerequisite, availability, engagement, and failure
   classification: rows 6-19 and 23-25.
3. Manual progress, stress, and demo reporting: rows 26-34.
4. Keep rows 42-47 as strict adapters until their external or live producer can
   supply an equivalent typed fact.

For each migration, the proof has two independent arms: hold printed text
fixed while changing the structured value and require the consumer to follow
the value; then remove the structured channel and require unknown/refusal while
the same prose remains. A consumer that reads a field but still follows text
has not been migrated.
