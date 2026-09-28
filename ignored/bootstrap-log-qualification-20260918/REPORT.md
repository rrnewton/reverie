# Reverie bootstrap logging component qualification

Commit **0630181218655dd9b0a1536b1ac8bf0dc35f794a**, tree b637a46acac9c0a72c7a300531f5c3cca2670062, branch codex/kvm-bootstrap-log-20260918, parent 7d863ab3f02639731713a01467b2548c41e3dbfb. No push or publication occurred.

The commit is the exact independently reviewed three-path proposal ff385e606cd4cf64f80c834fbd141c386e844df2e159efd06da54935f3e60f92: 226 insertions and one replacement in reverie/src/guest.rs, reverie-ptrace/src/task.rs and reverie-ptrace/src/tracer.rs. No source correction was needed after compilation or execution. It adds an explicit conservative Guest query, tracks the actual Command bootstrap lifetime, and uses the existing hostaddr marker for only the typed initial execve pointers. Raw unused registers, all later guest arguments and function-guest values remain unchanged.

## Actual results

All 11 bounded phases passed on their first execution: metadata, compilation, inventory, seven individual exact tests, and core checking. RESULTS.json binds their raw statuses, terminal results, CPU/wall measurements and unchanged-input readbacks.

* cargo test --offline --locked -p reverie-ptrace --lib --no-run --message-format=json completed with zero structured compiler diagnostics. Cargo emitted exactly one selected actual ELF, fresh=false, SHA256 258cc9667b311f6658cb840863a372ada1b62f9bde2075b0ddafbb77f06b404e, 164,660,176 bytes. Compilation used 113.778046 aggregate CPU seconds and 63.156081 observed wall seconds.
* Inventory contained 181 unique tests and every selected name exactly once. The three new controls and four original controls each executed exactly once with --exact --test-threads=1 --nocapture and libtest JSON: 1 passed, 0 failed, 0 ignored, 0 measured, 180 filtered per run. In total seven unique tests passed, not the full 181-test population. Their aggregate services used 15.687068 CPU seconds and 22.245155 summed observed wall seconds.
* cargo check --offline --locked -p reverie-core --all-targets --message-format=json passed with zero structured diagnostics: 35.383088 aggregate CPU seconds and 19.863494 observed wall seconds.

The new live controls execute a Command through two successful execs and verify provenance is false before each post-exec callback and for the second exec; execute spawn_fn's real getpid and verify false provenance; and compare exact formatter bytes, null handling, raw tail values, post-bootstrap behavior, other syscalls and alias distinctions. Existing controls cover explicit argv[0], exec-generation state, subscribed restart_syscall dispatch and the unsubscribed Linux result. Each control's full name and actual outcome is retained in RESULTS.json and controls/test-N/outcomes.json.

Each compile/check phase used the declared 600 CPU-second / 900 wall-second / 16 GiB / zero-swap / 16 MiB stderr bounds. Inventory and each exact test used 30 CPU seconds / 60 wall seconds / 16 GiB / zero swap / 1 MiB stderr. The unchanged observer authenticated actual service identity before release, retained complete final accounting, and the caller independently read inactive/empty service state twice afterward. All raw statuses are zero, all stderr outputs are untruncated, and no bound or cleanup error occurred.

## Input and commit binding

The installed repository-pinned nightly-2026-07-29 Cargo/Rustc/Rustdoc paths were used explicitly with jobs=2. A fresh owned target/bootstrap-log cache was created; no foreign target cache was reused. RUNNER_ORIGINS.json binds unchanged generic observer/lane-lease helpers. The lock seed alone was copied after verifying all 27 Cargo.toml files match its source; every Cargo invocation stayed --locked --offline. Metadata binds the 198-package relevant dependency closure. All 11,773 external source files (309,638,344 bytes), full tracked working source, config contents/absences, payload program, argv and environment were checked before and after relevant phases.

Tests ran before committing, against the exact working bytes recorded in source-manifest.json. COMMITTED_READBACK.json verifies every one of 2,593 tracked blobs in the resulting commit against that tested manifest; no source changed between test and commit. This is an explicit content-equivalence binding, not a claim that Cargo ran after HEAD changed.

Commit creation returned zero. The first broad porcelain assertion then reported the expected untracked qualification artifact directory: this fresh repository does not Git-ignore the name ignored/. The exact artifact-only list is retained. Tracked source and index are clean; no ignore/exclude rule was changed and no artifact was added to the commit. This readback correction did not change any product, test or acceptance oracle.

## Remaining limits

These are Reverie component results. No Hermit composed-head run, canonical INFO comparison, CHAOSRAND replay fix, full library run or optional native backend compilation is claimed. The general Guest default is source-compatible; the public API addition still requires its normal human-review consideration. AUTHOR_AUDIT.md records all implementors and the absence of a dynamic IntoGuest forwarding control.

The author cannot independently approve this commit. Final native and actual Claude review of this head remain coordinator gates before publication/landing. The exact truthful degraded identity tag and Task: kvm-lane-to-full-determinism-and-parity trailer are in the commit body; resolver UNRESOLVED remains disclosed. No identity infrastructure was changed.

The goalpost audit finds no weakened assertion, widened tolerance, new exemption/skip, relaxed comparator, relabeled failure or deleted check in the product diff. All original F2/F3 runtime failures remain preserved and open for their separate composed correction.
