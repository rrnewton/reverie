The source is the single-file cleanup correction frozen as source-v24 on landed c8f4. After the normal commit, a separate immutable binding will identify the same source bytes and final head. Earlier records remain unchanged. No compilation has started.

Use the existing qualification-build-v6 caller with the source/final-head bindings and fresh qualification-build-v7 outputs. Keep its actual compile command:

`cargo test --locked --offline -p reverie-kvm --lib --test static_elf --no-run --message-format=json`

Keep 600 aggregate CPU seconds / 900 wall seconds, 16 GiB memory / zero swap, two jobs, 16 MiB compile diagnostic/read bounds, existing observer f10ab861 and owned target/prejoin-native-v9 cache. The already retained v23 library and static ELFs must match their separate copied bytes before cache reuse. Stop on compilation failure and retain all structured stdout diagnostics and stderr. No tests run in this stage.

Keep exactly the two compiler-emitted library/static inventory invocations, each `--list --format terse`, under 5 CPU / 15 wall seconds and 1 MiB output bounds. Require every previously measured 453 library identity and the one newly authored child-cleanup method to appear once. Require every original 22 static integration identity and preserve the complete existing 288 static inventory. The resulting complete names and counts must be taken from those actual inventories, not from source estimates.

Then bind one current full-library service to its newly emitted and separately copied ELF and complete inventory. Reuse futex-full-library-v1's actual API 12 admission, original unfiltered `--test-threads=1 --nocapture -Z unstable-options --format=json` arguments, original JSON outcome parser, full per-name recording and terminal accounting checks. Keep 30 CPU / 60 wall seconds, 16 GiB / zero swap and 1 MiB fatal diagnostic/read bounds. Require REVERIE_REQUIRE_KVM=1 and no hardware skip; every actual registered method stays selected. The full invocation contains the previous 44 native and four VM methods, so successful completion replaces duplicate selected invocations for this source. Preserve any first failure and stop dependent stages; do not retry or increase the waiter deadline.

Reuse qualification-execution-v6's original static-elf-01 through static-elf-22 stages and exact order, hardware admission, per-method 30 CPU / 60 wall / 16 GiB / zero-swap / 1 MiB bounds, recursive output rules and assertions. Remove only its four preceding VM stages because the complete library already executes those methods. Bind actual new static ELF and inventory after compilation. Preserve all original integration files byte-for-byte, including ten exec diagnostic modes, leader exit, both fork-child writes, natural exit status, waitability, consuming hooks and distinct cleanup error expectations. A failure stops dependent stages and remains recorded.

Run the existing lint-v14 workspace format and all-feature Clippy commands with fresh lint-v15 outputs and the separate owned Clippy cache, unchanged resource limits and source/lock bindings:

`cargo fmt --all -- --check`

`cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`

Every accepted observed service requires real CPU accounting and fresh inactive/empty readback, plus full source/input/ELF checks before and afterward. No source or lock changes while runs or exact source reviews are active. Final source and actual-Claude reviews are required before normal public publication and rebase landing, without treating a whole-DAG receipt as a hard prerequisite. This validation remains Reverie component evidence; coordinated Hermit scheduler/guest qualification is owned separately by the root and summary worker.
