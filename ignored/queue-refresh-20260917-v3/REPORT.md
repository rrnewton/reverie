At 2026-09-17 20:24:47 UTC, the complete queues still contain 19 open Hermit pull requests and one open Reverie pull request. Relative to the retained 19:32 UTC v2 snapshot, none entered and none departed. Two Hermit entries changed head and returned base SHA; all other head/head-ref/base-ref/base-SHA/title/draft/state fields compared here are unchanged. Follow-up file and state reads finished at 20:25:36 UTC. This is queue and scope evidence, not source approval, ownership, test evidence or permission to land.

The separate explicit GETs of refs/heads/main returned:

| Repository | Current remote main |
| --- | --- |
| https://github.com/rrnewton/hermit | `c8c33db1e5b59aab9f7ead3c1c710bef0b68e2b6` |
| https://github.com/rrnewton/reverie | `7d863ab3f02639731713a01467b2548c41e3dbfb` |

These are direct remote-ref observations. The base objects in pull responses below are recorded separately and are not substitutes for a main-tip query. The 19:32 queue snapshot did not independently query main, so this report does not derive a main-tip change from its old PR base objects.

The complete open population is:

| Full URL and title | Exact head | State | Returned base |
| --- | --- | --- | --- |
| https://github.com/rrnewton/hermit/pull/3077 — Retry the hermetic fetch so one network blip cannot discard a 270-node run | `552c68b8f763e4fc431928b44e637b61d5d5d83a` | Open, non-draft | `main` / `6945e62a7f43610ac4c4559ceaae80110ad81794` |
| https://github.com/rrnewton/hermit/pull/3073 — Make a refused count say whether the tests it ran actually passed | `62cb9321b8c95cf184904d98cf03677fcbe8f2fb` | Open, non-draft | `main` / `f7e1d3ddecd57168245e60e232146acba5b1d677` |
| https://github.com/rrnewton/hermit/pull/3070 — Carry a scorecard write-back refusal's own words into the durable record | `cb7077228cb3308a1e2594d43add36100bab2232` | Open, non-draft | `main` / `6bd63ef1740a7680adfd0a6b4585942ec06aa2b8` |
| https://github.com/rrnewton/hermit/pull/3069 — Bind a compared cell verdict to the exact attempt it was computed from | `858a65e93d5ffe289bd6a329a67d62d4ecc42989` | Open, non-draft | `main` / `c8c33db1e5b59aab9f7ead3c1c710bef0b68e2b6` |
| https://github.com/rrnewton/hermit/pull/3066 — Report a node whose required results were never written as undetermined | `6752f558fdc6049326b763bdb63603762b54aa77` | Open, non-draft | `main` / `c8c33db1e5b59aab9f7ead3c1c710bef0b68e2b6` |
| https://github.com/rrnewton/hermit/pull/3030 — Run default compatibility checks in the pinned image | `934032fab5bcdde908bccdee6fd99effc6f15c35` | Open draft | `main` / `d0b1468b0964a16052bd32d90b9990aa0c9ace7a` |
| https://github.com/rrnewton/hermit/pull/3010 — Repair full-validation graph prerequisites | `1f26de2c6a8291076a87019c31fbb1037386282b` | Open, non-draft | `main` / `0ac76b5e2fc0ec0711a88f78231d6fe1d67858da` |
| https://github.com/rrnewton/hermit/pull/3009 — Preserve native diversity evidence | `e26c832e98e23eff3044c874467c0a991827c20f` | Open, non-draft | `main` / `0ac76b5e2fc0ec0711a88f78231d6fe1d67858da` |
| https://github.com/rrnewton/hermit/pull/2969 — Use harness-managed run evidence for verification | `e7af5cd8e5a80a0b74be5f44f26b33d4798c2697` | Open draft | `main` / `3111e75531c175e00dea7f6e1758bd9791019b82` |
| https://github.com/rrnewton/hermit/pull/2958 — Prepare harness-managed verify evidence | `f18b76cc464a3f8f7704f2d7abdbf53baff157e0` | Open draft | `main` / `500dded1b2c614694ba97bdafe2d196c8ed63eb4` |
| https://github.com/rrnewton/hermit/pull/2922 — Retain exact validation test results | `cefe94340d721d537f7afc9398eaa5a2285740a4` | Open, non-draft | `main` / `54ea4e3935a86cbb0257fcbe2debf4ba620e5035` |
| https://github.com/rrnewton/hermit/pull/2907 — demos: derive Demo 8's crash seed, and make a stale one say so | `68605f886745a060e0a2a8e24ab933815acc2d22` | Open, non-draft | `move-demos-directory-into-hermit` / `1eef513dc8a9d97d910206fe3f72bfb28a67ea62` |
| https://github.com/rrnewton/hermit/pull/2904 — demos: move the unified suite into Hermit | `33a8f546deac001a2ae41f1a1706a0c9f9fc9b85` | Open, non-draft | `main` / `6f2a46ffbf0854f99b10109f34b653ef86594cac` |
| https://github.com/rrnewton/hermit/pull/2836 — Apply SaBRe physical-exit reports during scheduler maintenance | `1948e95fe14cc95687e886704420a9922a1fe1cb` | Open, non-draft | `main` / `eec5e18affd17229f86b3943520ee2b9821a377d` |
| https://github.com/rrnewton/hermit/pull/2747 — Wait for physical thread exit before waking pthread joiners | `b3901a992c99b99a79e53adc877c8d4324b1e92e` | Open, non-draft | `main` / `bae4a34fec5bfe45dd8cad00adcd5b6ddb04abcc` |
| https://github.com/rrnewton/hermit/pull/2694 — Preserve inherited stdio flags and append writes across run and replay | `46670790208e71be0b1aa0e249297c0f4b38663a` | Open, non-draft | `main` / `24e15d63b2e356b878318faa968691ef3d5eb707` |
| https://github.com/rrnewton/hermit/pull/2368 — Revive the reproducible OSS Buck2 build | `26276e2b266e944bc02ff775b4ceb5cb0956019e` | Open draft | `main` / `d252f99acb2d790cc2f6ad2a2ed8c1c85d9ef7f7` |
| https://github.com/rrnewton/hermit/pull/2302 — Land M2 canonical verification and fix remaining measured reds | `d16a0f1c12626ac7991a354b9597b135633ea25c` | Open draft | `main` / `302a1a9c0fde564db0292d1ea5cabc91343e79bc` |
| https://github.com/rrnewton/hermit/pull/1689 — Honor DBT host log files across fork and exec | `96eeb0d603875e595f36d6ce97fc77a998ea53a2` | Open draft | `main` / `1a679bf948dabc7a4b4233c921254d70d839e477` |
| https://github.com/rrnewton/reverie/pull/467 — Admit DBT threads before executing guest instructions | `51272804a0073c60d3dc73dbd396ba664f934bf9` | Open draft | `main` / `0771253923fac12de6b1e1352227318548165c70` |

The two changes are confined to already known shared-validation proposals:

- https://github.com/rrnewton/hermit/pull/3066 advanced from `b27530786ae57fb9cc43d9d49563ce8f02e64013` to `6752f558fdc6049326b763bdb63603762b54aa77`, with returned main base `c8c33db1e5b59aab9f7ead3c1c710bef0b68e2b6`. Its complete current inventory is one file, `scripts/lib/validate_classification.rs`. Its unchanged body describes distinguishing unwritten structured test results from measured product failures. This affects interpretation of KVM validation results along with every other backend; it introduces no KVM runtime path in the current inventory. Its correctness and claimed controls were not reviewed here.
- https://github.com/rrnewton/hermit/pull/3069 advanced from `9cec06dcc9d8acf4828b72f73d9bb1abd1f01ddc` to `858a65e93d5ffe289bd6a329a67d62d4ecc42989`, with the same returned main base. Its body and all four returned per-file patch texts match the retained earlier query. Current files remain `ci/manifest-plan/src/ledger.rs`, `ci/manifest-plan/src/ledger/schema10.rs`, `ci/manifest-plan/src/ledger/schema10/tests.rs`, and `scripts/lib/validate_cell_results.rs`. Its scope remains binding compared cell verdicts to their selected attempt, relevant to KVM scorecard provenance and separate from backend implementation. Equality of the returned diff text does not assert complete head-tree identity or carry an old source approval to the new head.

For both, post-file-query PR metadata still named the initial snapshot head and changed_files equalled the complete returned inventory. Their titles, branches and draft state are unchanged. No new KVM-related proposal or new runtime scope was found. There was therefore no reason to re-download or re-review the unchanged donor diffs.

The established dispositions remain distinct:

- https://github.com/rrnewton/hermit/pull/2958 and https://github.com/rrnewton/hermit/pull/2969 remain the broad harness-managed verification donor and successor at their exact prior heads. Preserve the root's per-obligation accounting and same linear continuation; independently landed increments do not by themselves discharge the whole donors.
- https://github.com/rrnewton/hermit/pull/2694 remains the inherited-stdio/append work with its separately established startup-failure and open-file-description lifetime obligations. The later pre-join cleanup landing is not a new proof of those separate contracts.
- https://github.com/rrnewton/hermit/pull/2302 remains the M2 strict-default/lossy-removal obligation. The newly prepared M2 v5 preview is isolated, uncompiled and unlanded; it has not changed this public head or closed the obligation. Preserve numeric/INFO-divergence controls in the same continuation.
- https://github.com/rrnewton/hermit/pull/2747 remains the established SaBRe-only physical-exit work, distinct from KVM's pre-join failure repair. The separate SaBRe application https://github.com/rrnewton/hermit/pull/2836 is likewise unchanged.
- https://github.com/rrnewton/reverie/pull/467 remains the foreign DBT admission draft at `51272804a0073c60d3dc73dbd396ba664f934bf9`. Being the only open Reverie PR does not transfer it to this lane. The Hermit DBT host-logging proposal https://github.com/rrnewton/hermit/pull/1689 also remains distinct.

https://github.com/rrnewton/reverie/pull/578 was queried explicitly: state closed, merged true, PR head `12d4ce8c0bc426f1ae41416f5b4a699e2c300879`, merge commit `7d863ab3f02639731713a01467b2548c41e3dbfb`, merged at 2026-09-17T18:48:15Z. This agrees with the separately queried current Reverie main. The complete open list contains only the known foreign draft, so no own Reverie PR is left open at this observation. This public-state read does not replace the previously retained landing content proof.

All nine network operations were explicit GETs wholly wrapped in `/usr/bin/with-proxy`, with the established 15 CPU seconds per process, 60 wall seconds and 16 MiB per output-stream bounds, zero core files and normal inherited address space. All succeeded; stderr was empty. Each complete open list returned fewer than the requested 100 entries, so no second page was needed. The three earlier local runtime/cgo aborts under an added 1 GiB address-space cap remain untouched in the old v1 directory and were not repeated or used as remote evidence. No claim, PR comment, label, review, closure, source, index, ref, cache, package or runtime change was made. No build, test, VM or guest ran.

`QUEUE-DELTA.json` retains every current full URL, exact head, returned base and selected metadata change. `PREPARATION.json` binds the retained 19:32 snapshot, prior reports, reader and proxy skill. The raw responses and exact command receipts are adjacent. `READBACK.json` authenticates the retained outputs and scope inventories; `MANIFEST.json` binds this finished report and those artifacts. Root retains live TaskGraph authority and landing decisions.
