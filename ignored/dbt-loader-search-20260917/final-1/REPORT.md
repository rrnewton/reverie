# Reverie private-loader repair: committed component evidence

Commit066a0fb3585477ba8bbac106a515bf4771d2f809, tree2e0e33786d73674f51bdd95222df34a1364269e5, parenta28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4. Only seven owned paths changed. All2547 current tracked entries match the committed objects/modes; index/tracked tree are clean. Own untracked ignored evidence remains. Normal commit hooks stayed enabled. The exact generated author tag is preserved in commit-1/who-am-i.json and the commit body; its degraded identity status was not relabelled.

The fallback repairs a missing later DIRECT client dependency after the original complete search fails. Existing successes retain their order. It checks only top-level clients with parsed metadata, requires a matching direct DT_NEEDED SONAME, rejects slash-containing pathnames, and searches client RUNPATH without globally promoting it. The two original path searches retain their existing promotion behavior.

## Actual results

The unchanged private loader failed the direct-leaf graph with exit255 naming libdrsearch_leaf_20260917.so; the identical ELF loaded natively with marker107007. The corrected loader now exits0 with that same marker. The seven original baseline cases and three added boundaries produced these results (numbers are actual invocation exits, not renamed test verdicts):

|Case|Native|Original DR|Corrected DR|
|---|---|---|---|
|Later direct leaf|0,107007|255,missing leaf|0,107007|
|LD precedence|0,211011|0,211011|0,211011|
|Client-directory precedence|not this oracle|0,111011|0,111011|
|Missing direct leaf|1|255|255|
|Indirect-only leaf|1|255|255|
|Legacy RPATH precedence|0,107007|0,107007|0,107007|
|Ordered RUNPATH|0,107007|255|0,107007|
|Mixed direct fallback then unrelated indirect|1,OTHER_LEAF|255,first LEAF|255,OTHER_LEAF|
|$ORIGIN RUNPATH|0,107007|255|0,107007|
|Explicit relative dependency pathname|1|255|255|

Baseline13 and candidate22 individual invocations are retained in parent native-fixture-design/controls-scope-{1,2}/results.json, with separate raw stdout/stderr, marker values, elapsed time, source/ELF/command bindings and actual statuses. Every qualifying invocation met its fixed expectation; negative loader invocations remain failed executions. Candidate controller elapsed0.548s. The same compiled fixture ELF bytes are used for comparisons; the original install is the retained pinned build and the candidate is a normal local source build, not a compiler-isolated performance comparison.

Normal build.rs rebuilt DynamoRIO from source (MISS then PUBLISHED): key0aa6d84239b5a04b7cda124ebed4c7e3adc8b62f5b4c96011a9b971e90d6b0a4. Native CMake took27.71s at jobs4; complete targeted Cargo build0/55.698s. Full Cargo native stdout/stderr is copied under final-1/cargo-native-output. Actual drrun4e91181f and libdynamorio3dcc6fe7 paths/bytes/modes are bound in RUNTIME.json; all125 install entries were reread unchanged after tests.

Permanent integration suite: actual10/10 pass,0 ignored,0 filtered in2.00s. The real collection, all ten named results, exact commands and source before/after are in focused-tests-2. cargo fmt --all -- --check passed; cargo clippy --offline --locked -p reverie-dbt --all-targets --all-features passed. Whole focused controller0/17.827s. This is relevant-package Clippy, not a full-workspace test receipt.

## Preserved failures and limits

native-build-1 stopped before compilation: offline lock generation101 lacked quickcheck_macros. Normal with-proxy cargo fetch into the private copied cache succeeded0/1.168s, with existing public CA verification retained; the real generated ignored lock is SHA301cccef. No manifest, lock or source-cache receipt was invented or borrowed.

focused-tests-1 collected ten methods but all ten actually failed before fixture execution because Cargo supplies LD_LIBRARY_PATH. The new fixture now allows only that inherited variable because every native/DR child unconditionally replaces it with its case directory. Other LD_/DYNAMORIO_ overrides still refuse. This explicitly changes the initial setup precondition; it does not change a provider marker, dependency graph, timeout, expected loader failure, or existing product test. The original source/failures and exact test-only correction1903aa0a remain retained.

All owned compile/control/build/fetch/test/commit scopes are terminal, their recorded cgroups and controller generations absent. Actual memory/pids max, OOM/kill/group events were zero in every successful scope; before/after raw counters are retained. Builds/checks used4CPU/8GiB/0swap/1024tasks/900s, tiny controls2CPU/2GiB/0swap/256tasks/180s with10+2s per invocation. This is source plus native-loader evidence. No Hermit guest, Hermit pin change, main-green claim or publication occurred. Reverie must land before an isolated Hermit pin update and ordinary official original DBT test validation.
