∆# Fresh main-landings and open-pull-request audit

Audit anchor: `2026-08-30T12:23:41Z` UTC. Cutoff: `2026-08-29T12:23:41Z` UTC. The 24-hour interval is inclusive at both ends; newly opened pull requests use `createdAt >= cutoff`, and stale pull requests use `createdAt < cutoff`.

This audit was taken directly from the live GitHub repositories `rrnewton/hermit`, `rrnewton/reverie`, and `rrnewton/dev-hermit`. It did not use TaskGraph notes, coordinator summaries, or local remote-tracking refs as sources. Each merged pull request was checked against the `main` commit history and GitHub's commit-to-pull-request association endpoint. Commits on `main` with no associated pull request are listed separately. No pull request that was open at the anchor closed between the anchor and collection.

## Counts

| Repository | Bucket 1: merged PRs | Bucket 1: direct commits | Bucket 2: newly opened | Bucket 3: stale open |
| --- | ---: | ---: | ---: | ---: |
| `rrnewton/hermit` | 30 | 1 | 3 | 38 |
| `rrnewton/reverie` | 3 | 0 | 0 | 9 |
| `rrnewton/dev-hermit` | 23 | 30 | 4 | 11 |
| **Total** | **56** | **31** | **7** | **58** |

The three repositories contained 65 open pull requests at the anchor: 7 newly opened and 58 stale.

## Verdict on the owner's top-line requests

### 1. ci-hub improvements

**Verdict: materially more trustworthy, but not complete.**

The landed work now includes exact framework-owned test counts, typed producer results, canonical state-root selection, immutable tool-root execution for validation and post-run readers, explicit indeterminate outcomes for unavailable inputs, source commit/tree identities, and exact-SHA signal cross-checks. Checkable examples include dev-hermit PRs [#226](https://github.com/rrnewton/dev-hermit/pull/226), [#228](https://github.com/rrnewton/dev-hermit/pull/228), [#231](https://github.com/rrnewton/dev-hermit/pull/231), [#233](https://github.com/rrnewton/dev-hermit/pull/233), [#237](https://github.com/rrnewton/dev-hermit/pull/237), [#238](https://github.com/rrnewton/dev-hermit/pull/238), [#241](https://github.com/rrnewton/dev-hermit/pull/241), and [#244](https://github.com/rrnewton/dev-hermit/pull/244), plus direct commit [`b30574084d7d`](https://github.com/rrnewton/dev-hermit/commit/b30574084d7d958236c64d93aeb8d2fbe9a00622), which makes unavailable inputs indeterminate instead of successful.

The improvement is visible in current behavior: `validate-status --sha 850685f6... --json` returns `NOT-VALIDATED` with current ledger freshness and zero authoritative records, while the same command for `c98dc60b...` returns `VALIDATED` with exact 76/76 nodes and 2,194/2,194 tests. Missing evidence is no longer presented as success.

The remaining gaps are concrete. The checked-in scorecard is stale relative to the parent series, and the owner-watch immutable-tooling work was still open as dev-hermit PR [#247](https://github.com/rrnewton/dev-hermit/pull/247). Its subsequent exact-head review also found that the proposed bootstrap still trusted the mutable primary checkout's `origin` and followed symlinkable cache paths. The system is now better at exposing disagreement, but the artifacts do not yet all agree.

### 2. Activating apparently-green cells classified as red

**Verdict: not accomplished in this window; zero cells moved.**

Comparing Hermit `main` immediately before the window, `03e691bcf72f43f62b34b861d1958bc7e95caa21`, with anchor `main`, `850685f6b9f3ae0dbc5a35fd3ec9f2abe867d95a`, shows zero changes to any cell's `status`, `enabled`, or `measurement` fields across all 5,744 cells.

The current on-disk population at the anchor is:

- 5,744 total cells.
- 676 enabled cells.
- 304 cells marked `green` and 372 marked `red`; those words are still plan-membership vocabulary on main, not direct pass/fail outcomes.
- 5,068 cells marked `not-applicable`.
- The expected plan contains 307 cells from 240 tests: 244 ptrace, 56 SaBRe, 6 LiteInst, and 1 KVM.
- Among the 372 red cells, 362 are `never-measured`, 2 are `measured-and-passed`, and 8 are `diverged`.
- Among the 304 green cells, 301 are `measured-and-passed` and 3 are `diverged-unlocated`.
- One not-applicable cell is marked `diverged`; the remaining 5,067 are `never-measured`.

Hermit PR [#2798](https://github.com/rrnewton/hermit/pull/2798) landed the ability to repeat the red population, but did not promote any cells. Hermit PR [#2863](https://github.com/rrnewton/hermit/pull/2863) explicitly says its proposed scorecard refresh also leaves selection unchanged.

Hermit PR [#2856](https://github.com/rrnewton/hermit/pull/2856) did add one RUN 1563 observation to each of the 304 cells that were already green; that is evidence write-back, not activation. The two red cells already carrying a measured pass are `applications/example-timed-progress-bar verify/ptrace` and `c-programs/nanosleep-par verify/sabre`.

The apparent 307-versus-304 denominator mismatch is an observability defect, not three missing checks. The generator deliberately excludes `custom` rows from the comparable `cells.json` matrix while still requiring them in the expected plan and in validation results. The exact selected partition is `307 selected = 304 comparable + 3 custom`: `backend-parity-c/environment-and-workdir custom/ptrace`, plus `system-utils/clock-determinism custom/ptrace` and `custom/liteinst`. `SCORECARD.md` states this split, but `scorecard.rs check` reports only that 5,744 comparable cells are current, which made the complete selected denominator easy to misread.

### 3. Agreement among scorecard, ledger, receipts, and run records

**Verdict: not yet.**

The newest published full run at the anchor was RUN 1570, event `devbig014-1788089463-579143`. Its canonical ledger row records 76/76 nodes, 2,216/2,216 tests passed, 496 filtered, zero skipped nodes, zero retried nodes, and 1,286 seconds at Hermit commit `4733542cf689ac99061e83121f3e35d83ca881b8`. Anchor `main` commit `850685f6b9f3ae0dbc5a35fd3ec9f2abe867d95a` has the identical Git tree, `b5aee5b5aab653301b4cdfdd54d00a8f4bc6315a`, after the rebase landing, but it has no exact-SHA GitHub status.

At the older exact green `c98dc60b...`, the ledger and qualifying receipt do agree: RUN 1566 records 76/76 nodes, 2,194/2,194 tests, zero skipped nodes, and receipt digest `b3994e267c937a2661b793d91d4357f4691f1778e8060184d8a1e58c7fb0643a`; the series contains 308 exact rows. Its run handle is identity-matched through typed admission (`run_number=1566`, `ledger_event=true`, wrapper exit zero), but its own terminal `state` and `result` remain `unknown` because the service-result JSON is missing. The handle corroborates identity, not outcome; the ledger receipt remains the outcome authority. GitHub's `Local validation` status is absent rather than contradictory.

The larger disagreement is the scorecard projection. Hermit `main` still records projection source commit `f009825e2b0089daf786a9d4686caa3b1d8b27b2` with only 9 rows and a refresh timestamp of `2026-08-27T09:23:50Z`. The landed parent series at dev-hermit `fdcec0b4769fd93eea86ca0c42ad2c03a824bbc1` contains 13,422 rows. Hermit PR [#2863](https://github.com/rrnewton/hermit/pull/2863) attempts to refresh that projection, but an exact-head substantive review found that its recorded source identity does not yet uniquely determine the generated scorecard.

The cross-check now detects the stale projection rather than falsely reporting agreement. That is a trust improvement, not artifact reconciliation.

### 4. Efficient current validates and removal of spurious capping

**Verdict: partially accomplished.**

Full validation is capable of completing: RUN 1570 completed its 76-node, 2,216-test full profile in 1,286 seconds, or 21 minutes 26 seconds. Anchor `main` was tree-identical to that validated head. Specific false or insufficient limits were repaired:

- Hermit PR [#2845](https://github.com/rrnewton/hermit/pull/2845) raised only the 32-seed `fp-reduction` chaos timeout from 30 to 40 seconds without reducing the workload.
- Hermit PR [#2849](https://github.com/rrnewton/hermit/pull/2849) raised `every_record_container_site_classifies_a_child_fault_by_name` from 30 to 45 seconds while preserving its 12-process workload.
- Hermit PR [#2824](https://github.com/rrnewton/hermit/pull/2824) made nextest and timeout evidence live instead of buffered.
- Hermit PR [#2741](https://github.com/rrnewton/hermit/pull/2741) removed fixed shared ports and work directories that made concurrent runs collide.
- Hermit PR [#2842](https://github.com/rrnewton/hermit/pull/2842) moved manifest retries inside the test harness.

The broader concurrency and cap cleanup did not land. Hermit PR [#2825](https://github.com/rrnewton/hermit/pull/2825), which removes broad guest/KVM serialization, remains draft and conflicting. Hermit PR [#2833](https://github.com/rrnewton/hermit/pull/2833), which consolidates shared Cargo work, remains stacked on it. A full current tree can be measured, but exact-current-SHA green is still vulnerable to rebase SHA changes and a moving `main`; it is not yet routine.

The current timeout ladder is materially better bounded: nextest defaults to 15 seconds plus grace with named exceptions; manifest cells default to 15 seconds with a 10-second kill grace and explicit backend/cell overrides; DAG nodes have explicit wall limits; and ci-hub supplies a 3,600-second child deadline around a derived 3,239-second Hermit whole-run budget. A guard refuses a manifest node whose wall limit cannot contain two cell attempts plus cleanup grace. Focused iteration paths exist through validate `--only`, `--requalify-cell`, and manifest-cli `run <test-id>`. What is still missing is the requested `validate list quick|full|super`; `--show-plan` explicitly does not enumerate Rust test IDs or E2E cells, and the super workflow has a six-hour job limit but no internal `--run-timeout` on its validate invocation. Of the latest ten full runs, all finished, but only four were `VALIDATED`; six were `NEEDS-RERUN`. Completion is now routine, exact-tip hard green is not.

## Bucket 1: everything landed on main in the 24-hour window

### Hermit: 30 merged pull requests

- `exact-framework-test-counts` — https://github.com/rrnewton/hermit/pull/2839 — Records exact passed-test totals from framework-owned typed results; merged at `2026-08-29T17:20:11Z` as `4dbadc85679b619ca91551bbc4bc66c0611cc0f2`.
- `inner-manifest-retries` — https://github.com/rrnewton/hermit/pull/2842 — Keeps retries inside the manifest harness and preserves exact suite identities; merged at `2026-08-29T18:47:57Z` as `f2be3d81c9cb609443bd34c4611683e2bfccf728`.
- `recovered-no-result-writeback` — https://github.com/rrnewton/hermit/pull/2843 — Allows scorecard write-back after a narrowly typed recovered no-result; merged at `2026-08-29T20:01:12Z` as `afda47accaaf7b749c7faf4b57c0e1fdfcaeac39`.
- `fp-reduction-timeout-calibration` — https://github.com/rrnewton/hermit/pull/2845 — Raises only the 32-seed floating-point chaos cell's insufficient timeout; merged at `2026-08-29T20:42:01Z` as `b1067a5bcd5a9cf9f8d51e14b1e6d80fd20ca49d`.
- `incomplete-chaos-evidence` — https://github.com/rrnewton/hermit/pull/2846 — Prevents incomplete chaos populations from becoming false product divergences; merged at `2026-08-29T22:02:18Z` as `7ef8e12db45323e6010f3247f69abf793e669f9b`.
- `typed-verification-consumers` — https://github.com/rrnewton/hermit/pull/2826 — Moves eleven verification consumers from human banners to typed reports; merged at `2026-08-29T22:12:27Z` as `6c2f025cea8e85912cfd54ebc6cee3fca0f0e3dd`.
- `later-canonical-attempt-writeback` — https://github.com/rrnewton/hermit/pull/2847 — Preserves incomplete attempts while accepting a later canonical result; merged at `2026-08-29T23:12:13Z` as `20b265fa3eb580360ff93304d4790f5664a3705a`.
- `manifest-cell-cpu-accounting` — https://github.com/rrnewton/hermit/pull/2830 — Records CPU at the cell process that owns the work; merged at `2026-08-29T23:24:10Z` as `f117a75569bd41d8b00ff6046cd12d0e4be2f3ae`.
- `signal-stop-lint-coverage` — https://github.com/rrnewton/hermit/pull/2699 — Schedules the full signal-stop checker rather than only its narrow self-test; merged at `2026-08-29T23:44:41Z` as `4ebf9eb59969d7e2f78fef530d88656753d7dab4`.
- `unique-compat-port-and-workdir` — https://github.com/rrnewton/hermit/pull/2741 — Replaces shared fixed compatibility ports and directories with per-run resources; merged at `2026-08-30T00:19:45Z` as `3fdfb6b33f837f5b73c353b53258a3c645db3867`.
- `live-timeout-evidence` — https://github.com/rrnewton/hermit/pull/2824 — Streams nextest evidence and keeps exact-count checks beside the typed producer; merged at `2026-08-30T00:34:27Z` as `3047712bd0f5149bcf2037d492e89ec36b826645`.
- `repeat-red-population` — https://github.com/rrnewton/hermit/pull/2798 — Makes repetitions preserve explicit red-versus-green population selection; merged at `2026-08-30T00:54:14Z` as `e277d771f25d8ef7715ebc6a14f4d1f1a13bf00d`.
- `manual-hosted-validation` — https://github.com/rrnewton/hermit/pull/2821 — Restores manually dispatched, non-gating hosted validation from constructed DAG steps; merged at `2026-08-30T01:09:19Z` as `6db1771493e0e06d46da8489020642e49e52c903`.
- `typed-nextest-counts` — https://github.com/rrnewton/hermit/pull/2827 — Derives nextest executed and filtered counts from typed events; merged at `2026-08-30T01:12:21Z` as `9e418a1b25813081ff248919a1964272f3dfce91`.
- `shared-binary-serialization` — https://github.com/rrnewton/hermit/pull/2849 — Serializes relinking of shared integration binaries and calibrates one timeout; merged at `2026-08-30T01:26:56Z` as `d98da35f41bbf0dac6401fb3e689c34f3ae67c4a`.
- `shared-structured-results` — https://github.com/rrnewton/hermit/pull/2848 — Moves progress and compatibility reporting onto shared structured test results; merged at `2026-08-30T01:41:24Z` as `4bc89f875d4480188767bb8eff837fa5a1048ce6`.
- `validate-tool-state-separation` — https://github.com/rrnewton/hermit/pull/2850 — Executes validation tools from the launching checkout while retaining canonical shared state; merged at `2026-08-30T02:38:40Z` as `4dd40e777230c7321ec9d9a3b206ddc5f1e0e1f1`.
- `stop-fixture-isolation` — https://github.com/rrnewton/hermit/pull/2853 — Prevents signal-stop fixtures from inheriting the outer validation run and proves no nested scheduler starts; merged at `2026-08-30T03:42:47Z` as `bbde7e1e89cadac0233c87aad3166eff337e222b`.
- `exhaustive-local-enums` — https://github.com/rrnewton/hermit/pull/2851 — Makes five local enum consumers name every current variant; merged at `2026-08-30T03:52:44Z` as `de55a6fdf566108d50a84883cf0d3c7536368455`.
- `verdict-writeback-separation` — https://github.com/rrnewton/hermit/pull/2852 — Separates a validation verdict from a later scorecard-write failure; merged at `2026-08-30T04:00:10Z` as `27612b03bbc742b67e67eac18bf1adcf25bbb8bd`.
- `tool-root-negative-control` — https://github.com/rrnewton/hermit/pull/2854 — Makes the tool-root refusal self-test independent of `TMPDIR`; merged at `2026-08-30T05:10:10Z` as `1dc6cec4c46aa86764b516bd87b9522fbb156249`.
- `producer-owned-localhost-port` — https://github.com/rrnewton/hermit/pull/2855 — Makes the localhost fixture publish its own bound port without running `ss` inside the measured guest; merged at `2026-08-30T05:41:03Z` as `8f07f6b6d3ab184f0572655b319c5b6dbe434033`.
- `run1563-scorecard-evidence` — https://github.com/rrnewton/hermit/pull/2856 — Records the selected-cell evidence from green RUN 1563; merged at `2026-08-30T07:10:20Z` as `62e4a7ce3ddc8ffcc95ab059feb8dc8dedca3c3b`.
- `structured-detlog-results` — https://github.com/rrnewton/hermit/pull/2831 — Records structured DETLOG event results; merged at `2026-08-30T07:19:01Z` as `9e7bbd547228fa2e9399d9f7864b0922ac084760`.
- `scorecard-source-commit` — https://github.com/rrnewton/hermit/pull/2840 — Adds the immutable source commit to scorecard projections; merged at `2026-08-30T07:31:27Z` as `c98dc60b23b8bd7238a94c3a0f93cc425cf8c6fe`.
- `serialized-validation-contracts` — https://github.com/rrnewton/hermit/pull/2857 — Rebuilds the producer-owned validation contracts from PR 2811 on current main; merged at `2026-08-30T08:16:52Z` as `a8b65583a8c9a3e96913f58604ed6aa87fabcf69`.
- `producer-owned-runtime-facts` — https://github.com/rrnewton/hermit/pull/2859 — Serializes producer-owned runtime facts including backend engagement; merged at `2026-08-30T08:56:49Z` as `3502266e6f623aca5bb4c805c1d4b2eec22d6f8d`.
- `producer-owned-failure-results` — https://github.com/rrnewton/hermit/pull/2860 — Records producer-owned failure classes and e9patch results; merged at `2026-08-30T09:02:43Z` as `3b48f70eeaf8d4f2edd8d85e56ef56526ac6cc0c`.
- `scorecard-source-tree` — https://github.com/rrnewton/hermit/pull/2862 — Extends scorecard provenance with the exact source Git tree; merged at `2026-08-30T10:44:06Z` as `5e5c9eab1dc13d7a6eca0b865be4bead518d287f`.
- `kvm-stdin-replay` — https://github.com/rrnewton/hermit/pull/2802 — Replays identical stdin during KVM verification and repairs two current validation fixtures; merged at `2026-08-30T11:40:48Z` with tip `850685f6b9f3ae0dbc5a35fd3ec9f2abe867d95a`.

### Hermit: 1 direct main commit with no associated pull request

- `scorecard-fixture-dependencies` — https://github.com/rrnewton/hermit/commit/5cc77a59b9b3b46a1021d1accf58f95fb5e08ec2 — Keeps the scorecard fixture's declared dependencies complete; committed at `2026-08-29T17:49:50Z`.

### Reverie: 3 merged pull requests

- `exhaustive-local-enums` — https://github.com/rrnewton/reverie/pull/516 — Makes Reverie's local enum consumers exhaustive; merged at `2026-08-30T03:39:42Z` as `0284f6cb8b43343038e468f755b0dcda018938b5`.
- `per-gate-test-counts` — https://github.com/rrnewton/reverie/pull/513 — Records framework-owned test counts for each validation gate; merged at `2026-08-30T08:50:30Z` as `cbe9c4af8615f87137cb4363aa8c09b81d1b201e`.
- `libtest-json-counts` — https://github.com/rrnewton/reverie/pull/517 — Produces validation counts from libtest JSON rather than prose; merged at `2026-08-30T08:52:32Z` as `c2f45c842b3fea3fb29a00079b5f9abe487074ac`.

### dev-hermit: 23 merged pull requests

- `validate-history-scope` — https://github.com/rrnewton/dev-hermit/pull/225 — Labels branch-head versus main-history validation rows clearly; merged at `2026-08-29T14:58:59Z` as `6f78ca848294b6abe578bb1d7b3a6ef4e4c5b47e`.
- `commit-signal-crosscheck` — https://github.com/rrnewton/dev-hermit/pull/227 — Cross-checks retained CI signals at an exact commit; merged at `2026-08-29T16:23:21Z` as `0ab6b31effb0dc200fd88a9037c47cbea308239e`.
- `exact-test-count-display` — https://github.com/rrnewton/dev-hermit/pull/226 — Displays exact passed-test counts from Hermit's typed producer; merged at `2026-08-29T17:20:22Z` as `569f30a2d4538daa46a33f1579d901b17bbcf396`.
- `typed-retry-attempts` — https://github.com/rrnewton/dev-hermit/pull/214 — Classifies retained retry attempts from structured results; merged at `2026-08-29T17:54:22Z` as `030e67f18dbbd4571cdec41c31c7a0f0d909d3b8`.
- `prose-reader-audit` — https://github.com/rrnewton/dev-hermit/pull/219 — Documents remaining places that reconstruct test facts from output; merged at `2026-08-29T17:55:26Z` as `1ce3952a2b93b28b07ba0a54cab4fcf073f3f0a2`.
- `scheduler-prose-boundaries` — https://github.com/rrnewton/dev-hermit/pull/221 — Documents the remaining justified scheduler/output adapters; merged at `2026-08-29T17:56:12Z` as `41c7b6d4a7529ad3b59363e5a6bf3f578b9c8f84`.
- `typed-series-attempts` — https://github.com/rrnewton/dev-hermit/pull/230 — Classifies series rows from typed attempts rather than comparison-mode assumptions; merged at `2026-08-29T22:04:58Z` as `89d8d28eb2da4bf2631aad3a672fff7d4f47df45`.
- `reviewer-independence` — https://github.com/rrnewton/dev-hermit/pull/192 — Refuses merge authorization when both review lanes resolve to the same agent; merged at `2026-08-29T23:08:42Z` as `3ed4c8b68c2908c95c6a22aabaae8070a53e43e9`.
- `launcher-state-root-split` — https://github.com/rrnewton/dev-hermit/pull/231 — Passes immutable tool and canonical state roots separately to validate; merged at `2026-08-30T02:44:24Z` as `0cd832791d6b3f26cfd915ee793590c0f45e9563`.
- `exhaustive-parent-enums` — https://github.com/rrnewton/dev-hermit/pull/232 — Makes parent enum consumers exhaustive; merged at `2026-08-30T03:28:07Z` as `855b3973f6a6d1aebaaa56a60cdb5fe1337d6721`.
- `writeback-outcome-consumer` — https://github.com/rrnewton/dev-hermit/pull/233 — Consumes validation result separately from write-back outcome; merged at `2026-08-30T04:01:23Z` as `aeb2fd3fcd11c7cc50c201045849f81a5d4b90e7`.
- `health-pin-authority` — https://github.com/rrnewton/dev-hermit/pull/236 — Resolves health-check pins from current authority instead of stale parent `HEAD`; merged at `2026-08-30T04:36:18Z` as `cdcb3f688463acfb783e8a094682a5e5e619ace7`.
- `immutable-post-run-readers` — https://github.com/rrnewton/dev-hermit/pull/238 — Runs completion and attach readers from immutable validation tooling; merged at `2026-08-30T06:40:50Z` as `35d6362ac95732beb497f5d6ee09feec21787586`.
- `projection-stale-versus-incorrect` — https://github.com/rrnewton/dev-hermit/pull/228 — Distinguishes a strict series extension from a corrupted scorecard projection; merged at `2026-08-30T07:40:13Z` as `928e7e04564a0a1cce1789957d69fecf4f8c2bcc`.
- `serialized-contract-consumers` — https://github.com/rrnewton/dev-hermit/pull/239 — Consumes producer-owned validation contracts instead of reconstructing them; merged at `2026-08-30T08:29:21Z` as `e4322faf2351c60241a15228a4341f56c5d7b5f7`.
- `backend-engagement-consumer` — https://github.com/rrnewton/dev-hermit/pull/240 — Reads backend engagement from typed producer records; merged at `2026-08-30T08:59:35Z` as `240dd762bdf818f2aa6709b5acc094b7b2b106d9`.
- `failure-and-gate-consumers` — https://github.com/rrnewton/dev-hermit/pull/242 — Reads producer-owned failure and per-gate results across Hermit and Reverie; merged at `2026-08-30T09:06:32Z` as `e9de940b072b9150af4a21bec3bd6e319e78ee33`.
- `serialized-type-architecture` — https://github.com/rrnewton/dev-hermit/pull/243 — Documents ownership and boundaries for shared serialized types; merged at `2026-08-30T09:12:39Z` as `58e669ff8117de3dcf45e4c8783d507fcfa64ddb`.
- `canonical-signal-state` — https://github.com/rrnewton/dev-hermit/pull/241 — Makes signal-crosscheck read mutable validation state from the canonical parent; merged at `2026-08-30T09:15:59Z` as `2bc42f77c3b1cf6a61d83ddf5ee40d0f392fce3c`.
- `immutable-health-tooling` — https://github.com/rrnewton/dev-hermit/pull/237 — Runs health preflight from a verified content-addressed checkout; merged at `2026-08-30T09:27:55Z` as `d2db6871b82e578238e06182a11129f9723f79b8`.
- `scorecard-tree-admission` — https://github.com/rrnewton/dev-hermit/pull/244 — Verifies the scorecard's source series path, commit, tree, and run admission; merged at `2026-08-30T10:51:46Z` as `b1c8f50420ef557deb412bc9e1bc249324499591`.
- `publish-validation-series` — https://github.com/rrnewton/dev-hermit/pull/246 — Publishes the latest batch and advances the canonical series to 13,422 rows; merged at `2026-08-30T11:15:57Z` as `fdcec0b4769fd93eea86ca0c42ad2c03a824bbc1`.
- `owner-watch-refusal-records` — https://github.com/rrnewton/dev-hermit/pull/245 — Separates owner-watch launcher and service output and preserves typed refusal records; merged at `2026-08-30T11:45:57Z` as `745d01190e334151006033c9d1cb5ff75d74dfa7`.

### dev-hermit: 30 direct main commits with no associated pull request

- `ledger-append-2e7142` — https://github.com/rrnewton/dev-hermit/commit/2e714214cb82b030d8ce1ca9a97df2eca7903b60 — Appends one canonical validation event at `2026-08-30T12:01:21Z`.
- `ledger-append-aec143` — https://github.com/rrnewton/dev-hermit/commit/aec14309160dee7e353305383e98c717f6da6b9f — Appends one canonical validation event at `2026-08-30T11:01:29Z`.
- `series-append-d65ef0` — https://github.com/rrnewton/dev-hermit/commit/d65ef07a8f63467f29518e583b70600eec12913c — Appends 5,231 retained series events at `2026-08-30T09:42:37Z`.
- `ledger-append-2b07e4` — https://github.com/rrnewton/dev-hermit/commit/2b07e4fea3fbb610a28df01ee57dc90369f004d0 — Appends one canonical validation event at `2026-08-30T09:41:08Z`.
- `ledger-append-bb86ec` — https://github.com/rrnewton/dev-hermit/commit/bb86ece726ecef409a9950e72882dec720330cc5 — Appends two canonical validation events at `2026-08-30T09:01:20Z`.
- `ledger-append-c316c1` — https://github.com/rrnewton/dev-hermit/commit/c316c1efeb35215f743e816231b94fbf90579265 — Appends one canonical validation event at `2026-08-30T08:01:24Z`.
- `ledger-append-aea560` — https://github.com/rrnewton/dev-hermit/commit/aea5600a4e9f02c0871d8aec7d4bfc10c37607b6 — Appends four canonical validation events at `2026-08-30T07:01:50Z`.
- `unavailable-means-indeterminate` — https://github.com/rrnewton/dev-hermit/commit/b30574084d7d958236c64d93aeb8d2fbe9a00622 — Changes seventeen ci-hub readers from false success to explicit indeterminate at `2026-08-30T06:01:15Z`.
- `pin-localhost-repair` — https://github.com/rrnewton/dev-hermit/commit/36444cb9a7cdda0b2d5a6f19e6cdc36467cbf4dc — Advances the Hermit gitlink to the localhost fixture repair at `2026-08-30T05:43:32Z`.
- `pin-stop-fixture-repair` — https://github.com/rrnewton/dev-hermit/commit/c1f25ede4958f4289a1b34a7a5660b5e8fdb9c6e — Advances Hermit to the validation fixture-isolation repair at `2026-08-30T03:44:36Z`.
- `pin-validated-tip` — https://github.com/rrnewton/dev-hermit/commit/ee908fd4d010733895e22c06b00c693e05b3e8ac — Advances the Hermit gitlink to the then-validated tip at `2026-08-30T01:36:12Z`.
- `ledger-append-fe66a4` — https://github.com/rrnewton/dev-hermit/commit/fe66a490aed0339d76cce51507a5f51df086233d — Appends three canonical validation events at `2026-08-30T00:01:37Z`.
- `typed-e9patch-consumer` — https://github.com/rrnewton/dev-hermit/commit/8f33f5d1142b4a68b96639910ceb69195c8ef494 — Reads e9patch verification from a typed report at `2026-08-29T23:54:33Z`.
- `typed-landing-counts` — https://github.com/rrnewton/dev-hermit/commit/052572314e2cf764011e3a0f33bd662c0197144a — Reads landing-preflight counts from typed ledger events at `2026-08-29T23:46:39Z`.
- `functional-prose-reader-audit` — https://github.com/rrnewton/dev-hermit/commit/8043d9320425777570ede9bb93de53edf5466715 — Documents the remaining functional prose readers at `2026-08-29T23:24:44Z`.
- `pin-notrun-writeback` — https://github.com/rrnewton/dev-hermit/commit/a524bf4d9c2ade8aa9990e3c9ec695a1573a8633 — Advances Hermit to the recovered-NotRun scorecard repair at `2026-08-29T23:13:39Z`.
- `ledger-append-008345` — https://github.com/rrnewton/dev-hermit/commit/0083455239d838b7d3c20c063083e1b2eaab513b — Appends three canonical validation events at `2026-08-29T23:01:38Z`.
- `ledger-append-bc780f` — https://github.com/rrnewton/dev-hermit/commit/bc780f24565de6d5f916d865626345de4b86ee4b — Appends one canonical validation event at `2026-08-29T22:01:24Z`.
- `ledger-append-74c929` — https://github.com/rrnewton/dev-hermit/commit/74c9290ba55139df103aaefa0a97f338b6e06ee5 — Appends one canonical validation event at `2026-08-29T20:59:23Z`.
- `pin-fp-timeout` — https://github.com/rrnewton/dev-hermit/commit/16cfcfe67b9b7fe2e221865d52129a41cc427023 — Advances Hermit to the floating-point timeout calibration at `2026-08-29T20:42:32Z`.
- `archive-worktree-slots` — https://github.com/rrnewton/dev-hermit/commit/b4aa733fa8f76c4dbbf9f0372341d9318653998e — Archives reclaimed persistent worktree slots at `2026-08-29T20:23:44Z`.
- `pin-recovered-no-result` — https://github.com/rrnewton/dev-hermit/commit/b753c8a583dbe6c8a36f9eda26b7415f8003c515 — Advances Hermit to recovered-no-result write-back support at `2026-08-29T20:02:04Z`.
- `ledger-append-a0533a` — https://github.com/rrnewton/dev-hermit/commit/a0533af3918d561ae6ad312dfa2c0d3cde2c43a4 — Appends two canonical validation events at `2026-08-29T19:59:23Z`.
- `subreaper-test-isolation` — https://github.com/rrnewton/dev-hermit/commit/d95525a5ade394f4e8832756cafb6d53ca1c8ac8 — Isolates child-subreaper tests from parallel children at `2026-08-29T19:44:04Z`.
- `ledger-append-c5d213` — https://github.com/rrnewton/dev-hermit/commit/c5d213780a790ac2bed7887bbfc336e5ea5e4388 — Appends one canonical validation event at `2026-08-29T18:59:21Z`.
- `pin-local-retries` — https://github.com/rrnewton/dev-hermit/commit/838456de240fa9f391788edb6dea80b642b44f33 — Advances Hermit to local retries and exact identities at `2026-08-29T18:49:14Z`.
- `pin-manifest-gate-repair` — https://github.com/rrnewton/dev-hermit/commit/0d76b93bb5e9f123af8f640a5ef17fdda7afe7df — Advances Hermit to the manifest-gate repair at `2026-08-29T18:00:46Z`.
- `ledger-append-f54655` — https://github.com/rrnewton/dev-hermit/commit/f54655706cf3e10b92b48caf359e1085a39b4495 — Appends one canonical validation event at `2026-08-29T17:59:49Z`.
- `exact-worktree-tooling` — https://github.com/rrnewton/dev-hermit/commit/7a276e1599628bb252e0a58a3dec849e8ad77be8 — Makes worktree setup use exact validation tooling at `2026-08-29T17:31:11Z`.
- `explicit-worktree-branches` — https://github.com/rrnewton/dev-hermit/commit/74e074986e39e32caa98106b9b7c61c60bede441 — Passes validation worktree branches explicitly at `2026-08-29T17:26:09Z`.

## Bucket 2: newly opened pull requests

### Hermit: 3

- `scorecard-series-projection` — https://github.com/rrnewton/hermit/pull/2863 — Regenerates scorecard evidence from the landed 13,422-row parent series without changing selection. Created `2026-08-30T11:23:57Z` (59 minutes old); non-draft, clean and mergeable, with no formal review decision, but an exact-head substantive changes-requested comment says the recorded source does not uniquely determine the generated scorecard.
- `manifest-cpu-scorecard-salvage` — https://github.com/rrnewton/hermit/pull/2844 — Preserves one abandoned generated `cells.json` for later triage and explicitly does not propose landing it. Created `2026-08-29T20:11:14Z` (16 hours 12 minutes old); draft, conflicting, with no checks or reviews.
- `repeated-red-reporting` — https://github.com/rrnewton/hermit/pull/2838 — Separates terminal passes, clean passes, retries, and recovered repetitions in pressure-test reports. Created `2026-08-29T15:29:10Z` (20 hours 54 minutes old); draft and conflicting, with an exact-head high finding about the requalification selector.

### Reverie: 0

No Reverie pull request still open at the anchor was created in the preceding 24 hours.

### dev-hermit: 4

- `immutable-owner-watch` — https://github.com/rrnewton/dev-hermit/pull/247 — Moves the hourly owner-checkout watcher onto verified immutable tooling. Created `2026-08-30T12:18:13Z` (5 minutes old); non-draft, clean and mergeable, with no review or checks recorded by the anchor.
- `replay-time-policy-consumers` — https://github.com/rrnewton/dev-hermit/pull/235 — Makes receipt and series consumers honor replay's declared virtual-time policy. Created `2026-08-30T03:41:10Z` (8 hours 42 minutes old); draft, conflicting, with no checks or reviews.
- `scorecard-vocabulary-compatibility` — https://github.com/rrnewton/dev-hermit/pull/234 — Allows signal-crosscheck to read both old and proposed scorecard membership words. Created `2026-08-30T03:18:53Z` (9 hours 4 minutes old); draft, conflicting, with no checks or reviews.
- `worktree-artifact-salvage` — https://github.com/rrnewton/dev-hermit/pull/229 — Preserves sixteen files from unused worktree slots for later triage. Created `2026-08-29T20:09:29Z` (16 hours 14 minutes old); draft, clean and mergeable, with no checks or reviews.

## Bucket 3: stale open pull requests

### Hermit: 38

- `sabre-shutdown-order` — https://github.com/rrnewton/hermit/pull/2836 — Makes SaBRe's shutdown and exit ordering deterministic. Created `2026-08-29T10:07:06Z` (1 day 2 hours old); it remains open because the draft combines older stacked PRs and now conflicts with main despite earlier exact-head review and validation.
- `cargo-work-conservation` — https://github.com/rrnewton/hermit/pull/2833 — Moves shared Cargo compilation into the producer step. Created `2026-08-29T05:35:35Z` (1 day 6 hours old); it remains a draft based on PR 2825's non-main branch and cannot land independently.
- `independent-guest-concurrency` — https://github.com/rrnewton/hermit/pull/2825 — Removes broad guest and KVM serialization while retaining declared resource limits. Created `2026-08-29T01:11:31Z` (1 day 11 hours old); it remains draft and conflicting and needs a current-main rebuild.
- `typed-dagrun-termination-consumer` — https://github.com/rrnewton/hermit/pull/2822 — Consumes typed OOM and timeout facts instead of reason text. Created `2026-08-29T00:12:47Z` (1 day 12 hours old); its own current-main sweep says not to land the old conflicting stack and to extract only the remaining consumer migration.
- `agent-utils-pin-bundle` — https://github.com/rrnewton/hermit/pull/2812 — Pins wrkslots, resource caps, and command types. Created `2026-08-28T15:06:13Z` (1 day 21 hours old); it conflicts with main and has no current review or check evidence.
- `replay-mode-verdict` — https://github.com/rrnewton/hermit/pull/2810 — Reports replay matches under replay's `virtualize_time=false` policy. Created `2026-08-28T08:02:55Z` (2 days 4 hours old); it is clean but explicitly waits for parent consumer PR 235, which remains draft and conflicting.
- `flatten-strict-compat-dag` — https://github.com/rrnewton/hermit/pull/2800 — Replaces the generated nested compatibility graph with committed outer DAG nodes. Created `2026-08-28T03:05:11Z` (2 days 9 hours old); its head conflicts with current main and has no current review or check evidence.
- `mountinfo-identity-determinization` — https://github.com/rrnewton/hermit/pull/2799 — Canonicalizes mountinfo identity fields. Created `2026-08-28T02:46:19Z` (2 days 9 hours old); an explicit comment defers it because source provenance remains unresolved.
- `validation-outcome-taxonomy` — https://github.com/rrnewton/hermit/pull/2797 — Separates product failures from infrastructure, prerequisite, and no-result outcomes. Created `2026-08-27T23:51:13Z` (2 days 12 hours old); it targets a non-main stacked base and still depends on the old receipt-coverage stack.
- `ffb-nine-measurement-salvage` — https://github.com/rrnewton/hermit/pull/2783 — Mechanically preserves an abandoned measurement tree. Created `2026-08-27T16:18:41Z` (2 days 20 hours old); it remains an untriaged draft salvage artifact and conflicts with main.
- `validate-fix-audit-salvage` — https://github.com/rrnewton/hermit/pull/2782 — Preserves an abandoned validate and scorecard audit checkout. Created `2026-08-27T16:15:18Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-fresh-sv0-salvage` — https://github.com/rrnewton/hermit/pull/2779 — Preserves one abandoned validation checkout. Created `2026-08-27T16:03:39Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-fresh-ms2-salvage` — https://github.com/rrnewton/hermit/pull/2778 — Preserves another abandoned validation checkout. Created `2026-08-27T16:03:07Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-fresh-kix-salvage` — https://github.com/rrnewton/hermit/pull/2777 — Preserves another abandoned validation checkout. Created `2026-08-27T16:02:36Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-fresh-srj-salvage` — https://github.com/rrnewton/hermit/pull/2772 — Preserves another abandoned validation checkout. Created `2026-08-27T15:59:59Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-fresh-r9j-salvage` — https://github.com/rrnewton/hermit/pull/2771 — Preserves another abandoned validation checkout. Created `2026-08-27T15:59:29Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-fresh-qdx-salvage` — https://github.com/rrnewton/hermit/pull/2770 — Preserves another abandoned validation checkout. Created `2026-08-27T15:58:58Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `port-2255-salvage` — https://github.com/rrnewton/hermit/pull/2765 — Preserves the abandoned `port-2255` checkout. Created `2026-08-27T15:56:21Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `pr2736-validation-salvage` — https://github.com/rrnewton/hermit/pull/2764 — Preserves an abandoned PR 2736 validation checkout. Created `2026-08-27T15:56:13Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `pr2736-head-salvage` — https://github.com/rrnewton/hermit/pull/2763 — Preserves the abandoned PR 2736 implementation head. Created `2026-08-27T15:55:41Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `linuxboot-salvage` — https://github.com/rrnewton/hermit/pull/2758 — Preserves uncommitted Linux-boot work. Created `2026-08-27T15:52:20Z` (2 days 20 hours old); it is mergeable but remains an explicitly untriaged draft salvage artifact.
- `detlogdiff-salvage` — https://github.com/rrnewton/hermit/pull/2757 — Preserves uncommitted DETLOG-diff work. Created `2026-08-27T15:52:12Z` (2 days 20 hours old); it is mergeable but remains an explicitly untriaged draft salvage artifact.
- `pthread-exit-join-ordering` — https://github.com/rrnewton/hermit/pull/2747 — Delays joiner wakeup until physical SaBRe thread exit. Created `2026-08-27T12:09:19Z` (3 days old); it is clean but has a post-facto-review label and no current review or check evidence.
- `ppoll-sabre-measurement-report` — https://github.com/rrnewton/hermit/pull/2745 — Adds a dated report of thirteen measured SaBRe ppoll divergences. Created `2026-08-27T11:26:29Z` (3 days old); it is clean but has no review or check evidence and has not been picked up.
- `scorecard-selected-vocabulary` — https://github.com/rrnewton/hermit/pull/2738 — Renames misleading green and red plan-membership words to selected and not-selected. Created `2026-08-27T10:35:29Z` (3 days 1 hour old); it waits for parent compatibility PR 234.
- `red-cell-divergence-points` — https://github.com/rrnewton/hermit/pull/2717 — Records first-divergence points from an old 371-cell pressure pass. Created `2026-08-27T05:22:24Z` (3 days 7 hours old); its generated scorecard snapshot is draft and conflicting and requires remeasurement and rebase.
- `stdio-status-flag-containment` — https://github.com/rrnewton/hermit/pull/2694 — Prevents guests from mutating Hermit's inherited stdio status flags. Created `2026-08-26T15:50:09Z` (3 days 20 hours old); it conflicts with main and retains an exact-head high objection about incomplete `O_APPEND` semantics.
- `pressure-sandbox-denials` — https://github.com/rrnewton/hermit/pull/2427 — Classifies retained BPFJailer denial output. Created `2026-08-24T20:15:54Z` (5 days 16 hours old); its previously reviewed head now conflicts with main and needs rebase and revalidation.
- `record-replay-file-state-salvage` — https://github.com/rrnewton/hermit/pull/2425 — Preserves an interrupted SaBRe record/replay test. Created `2026-08-24T20:15:20Z` (5 days 16 hours old); it is draft and conflicting and a review comment names a blocking SaBRe defect.
- `kvm-envelope-salvage` — https://github.com/rrnewton/hermit/pull/2419 — Preserves a snapshot that enabled canonical KVM verification and promoted six cells. Created `2026-08-24T20:15:05Z` (5 days 16 hours old); it is a stale conflicting member of a four-snapshot duplicate cluster.
- `kvm-measurement-salvage` — https://github.com/rrnewton/hermit/pull/2408 — Preserves canonical KVM measurement work and a historical report. Created `2026-08-24T20:14:33Z` (5 days 16 hours old); it is draft and conflicting and overlaps three other stale snapshots.
- `compat-stability-aggregator-salvage` — https://github.com/rrnewton/hermit/pull/2400 — Preserves compatibility aggregation work. Created `2026-08-24T20:14:08Z` (5 days 16 hours old); it is draft and conflicting and review found a real aggregator defect.
- `oss-buck2-revival` — https://github.com/rrnewton/hermit/pull/2368 — Reconstructs the public Buck2 build. Created `2026-08-22T14:32:57Z` (7 days 21 hours old); it remains draft because the Hermit binary does not link and a cross-repository dependency is unfinished.
- `m2-canonical-verification-snapshot` — https://github.com/rrnewton/hermit/pull/2302 — Bundles old M2 verification and scheduler work. Created `2026-08-16T18:02:00Z` (13 days 18 hours old); it is draft and conflicting, and review says not to land the stack or extract the previously suggested commit.
- `robust-futex-owner-death` — https://github.com/rrnewton/hermit/pull/2223 — Repairs ptrace robust-futex owner death and related lifecycle evidence. Created `2026-08-13T02:59:52Z` (17 days 9 hours old); it remains draft and conflicting and requires a named co-update to avoid turning a green test red.
- `exact-head-review-gate` — https://github.com/rrnewton/hermit/pull/2216 — Binds core-review approval to the current pull-request head. Created `2026-08-13T01:58:13Z` (17 days 10 hours old); it is mergeable but retains an explicit unresolved P1 objection in its gate self-check.
- `blocking-pipe-determinism` — https://github.com/rrnewton/hermit/pull/2176 — Makes Detcore-created blocking pipe writes deterministic. Created `2026-08-12T10:12:08Z` (18 days 2 hours old); it conflicts with main and its review history still records unresolved exact-head objections.
- `dbt-log-routing` — https://github.com/rrnewton/hermit/pull/1689 — Adds DBT log-file routing, wait-status handling, heap DETLOG, and guest-scoped verification output. Created `2026-08-06T13:55:40Z` (23 days 22 hours old); it is draft and conflicting and review requires a hand-port split rather than rebasing the whole branch.

### Reverie: 9

- `liteinst-dispatch-count-types` — https://github.com/rrnewton/reverie/pull/512 — Replaces positional LiteInst dispatch arrays with named producer-owned counters. Created `2026-08-28T02:53:23Z` (2 days 9 hours old); it is approved and mergeable but remains draft and has not been picked up.
- `dbt-signal-id-salvage` — https://github.com/rrnewton/reverie/pull/479 — Preserves abandoned DBT queued-signal identity work. Created `2026-08-24T20:15:50Z` (5 days 16 hours old); it is an unreviewed draft salvage branch that conflicts with main.
- `dbt-clone-callback-salvage` — https://github.com/rrnewton/reverie/pull/478 — Preserves abandoned process-clone callback and DBT identity work. Created `2026-08-24T20:15:47Z` (5 days 16 hours old); it is an unreviewed draft salvage branch that conflicts with main.
- `backend-maturity-gate` — https://github.com/rrnewton/reverie/pull/476 — Adds fail-closed B0, B1, and B1.5 backend-maturity evidence. Created `2026-08-24T20:15:43Z` (5 days 16 hours old); it is draft and conflicting and has a blocking review because the parent consumer still requires the old seven-gate set.
- `dbt-lifecycle-salvage` — https://github.com/rrnewton/reverie/pull/467 — Preserves DBT virtual-identity and thread-start lifecycle work. Created `2026-08-22T14:22:44Z` (7 days 22 hours old); review says not to merge or rebase the snapshot and to extract a four-file change instead.
- `kvm-signal-salvage` — https://github.com/rrnewton/reverie/pull/463 — Preserves abandoned KVM signal and teardown changes. Created `2026-08-20T02:15:57Z` (10 days 10 hours old); it is an unreviewed draft salvage branch that conflicts with main.
- `warnings-and-strict-typing` — https://github.com/rrnewton/reverie/pull/442 — Applies warning denial and strict Python typing across Reverie. Created `2026-08-12T06:31:04Z` (18 days 5 hours old); it is draft and conflicting and its recorded blocker is the owner-level Actions and required-check configuration.
- `backend-output-api` — https://github.com/rrnewton/reverie/pull/434 — Adds captured output to the shared `Backend` trait. Created `2026-08-11T06:13:33Z` (19 days 6 hours old); it is draft and conflicting, requires an owner API decision, and lacks runtime proof for all three implementations.
- `merge-gate-no-result` — https://github.com/rrnewton/reverie/pull/404 — Makes a cancelled merge gate report no-result explicitly. Created `2026-08-07T07:42:31Z` (23 days 4 hours old); it remains draft with a failing gate and requires owner changes to both the registered gate blob and repository Actions configuration.

### dev-hermit: 11

- `worktree-elimination-salvage` — https://github.com/rrnewton/dev-hermit/pull/212 — Preserves the state of hundreds of reclaimed worktrees. Created `2026-08-28T20:53:15Z` (1 day 15 hours old); it remains an explicitly non-landing draft salvage artifact without review.
- `legacy-checkout-salvage` — https://github.com/rrnewton/dev-hermit/pull/204 — Preserves ignored and legacy flat-checkout state before deletion. Created `2026-08-27T16:48:31Z` (2 days 19 hours old); it remains an untriaged draft salvage artifact.
- `qemu-demo-salvage` — https://github.com/rrnewton/dev-hermit/pull/203 — Preserves QEMU demo and review artifacts. Created `2026-08-27T16:11:29Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `scheduler-experiment-salvage` — https://github.com/rrnewton/dev-hermit/pull/202 — Preserves an abandoned scheduler-switch experiment. Created `2026-08-27T15:54:06Z` (2 days 20 hours old); it is mergeable but explicitly untriaged salvage.
- `attribution-checkout-salvage` — https://github.com/rrnewton/dev-hermit/pull/201 — Preserves a checkout whose index already contained conflicts. Created `2026-08-27T15:53:41Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting by construction.
- `safehermit-build-salvage` — https://github.com/rrnewton/dev-hermit/pull/200 — Preserves abandoned safehermit build files. Created `2026-08-27T15:52:47Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `private-test-root-salvage` — https://github.com/rrnewton/dev-hermit/pull/199 — Preserves abandoned private-test-root work. Created `2026-08-27T15:52:34Z` (2 days 20 hours old); it remains draft, unreviewed, and conflicting.
- `validate-start-check-regression` — https://github.com/rrnewton/dev-hermit/pull/198 — Adds the missing regression test for forwarding `check` through validation startup. Created `2026-08-26T15:55:08Z` (3 days 20 hours old); it is clean and mergeable but has no review or checks and was simply not picked up.
- `ci-hub-source-freshness` — https://github.com/rrnewton/dev-hermit/pull/184 — Makes validate-status disclose whether the executed ci-hub matches main. Created `2026-08-25T23:29:08Z` (4 days 12 hours old); it now conflicts with main and has no current review or check evidence.
- `review-marker-grammar` — https://github.com/rrnewton/dev-hermit/pull/183 — Prevents ordinary English prose from being parsed as a changes-requested marker. Created `2026-08-25T22:59:07Z` (4 days 13 hours old); it now conflicts with main and has no current review or check evidence.
- `irrecoverable-pr-tip-sweeps` — https://github.com/rrnewton/dev-hermit/pull/177 — Schedules both PR-tip reachability and rescue-ref reconciliation checks. Created `2026-08-25T19:39:17Z` (4 days 16 hours old); it has one old Codex approval label but conflicts with main and lacks current-head validation and review.

## Exact query forms

The repository lists and details were obtained with these forms, replacing `OWNER/REPO`, `SHA`, and `NUMBER` as appropriate:

```bash
with-proxy gh pr list -R OWNER/REPO --state merged --limit 1000 \
  --search 'base:main merged:>=2026-08-29T12:23:41Z merged:<=2026-08-30T12:23:41Z' \
  --json number,title,url,mergedAt,mergeCommit,headRefOid,body,files

with-proxy gh api --paginate \
  'repos/OWNER/REPO/commits?sha=main&since=2026-08-29T12:23:41Z&until=2026-08-30T12:23:41Z&per_page=100'

with-proxy gh api -H 'Accept: application/vnd.github+json' \
  repos/OWNER/REPO/commits/SHA/pulls

with-proxy gh pr list -R OWNER/REPO --state open --limit 1000 \
  --json number,title,url,createdAt,updatedAt,isDraft,mergeable,mergeStateStatus,reviewDecision,headRefOid,baseRefName,labels,statusCheckRollup,latestReviews,reviewRequests,body,files,comments

with-proxy gh pr view NUMBER -R OWNER/REPO \
  --json number,title,url,createdAt,updatedAt,isDraft,mergeable,mergeStateStatus,reviewDecision,headRefOid,baseRefName,labels,statusCheckRollup,latestReviews,reviewRequests,body,files,comments
```

The cell comparison used the GitHub raw contents API for `ci/compat-envelope/cells.json` at pre-window commit `03e691bcf72f43f62b34b861d1958bc7e95caa21` and anchor commit `850685f6b9f3ae0dbc5a35fd3ec9f2abe867d95a`, keyed by `test|mode|backend`. The RUN 1570 figures came from the canonical `ledger/hermit/devbig014/2026-08.jsonl` committed on dev-hermit `main` at `2e714214cb82b030d8ce1ca9a97df2eca7903b60`.
