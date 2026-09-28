# Bounded strict Clippy corrections

Source is released for independent review and fresh qualification. This packet changes only `reverie-kvm/src/vm.rs`, `reverie-kvm/src/runtime.rs`, and `reverie-kvm/src/entry/driver.rs`. No formatter, compiler, Clippy invocation, or guest/test execution ran during this author pass. The earlier 231 passing declarations apply to the frozen before-source, not these edits.

Repository: `/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918`, branch `codex/kvm-parity-land-20260918`, Git base `91110d249ffd8957267d71fab8c83d9636105efe`. Before editing, each assigned file matched the named record in the qualified v3 source manifest, SHA-256 `69cce12356eb02bd7f7b0de7c529347ab3cf5cd6b1340a42551d227effe72ebf`. The immediate comparison is the frozen v3 file content, not the much earlier Git base. `before/` and `after/` contain the three complete files; `changes.patch` is their complete difference. Other authors own other files.

## Changes and preserved behavior

1. `entry/driver.rs:403`: collapse the terminal-receipt nested conditions into a let chain. The optional terminal is still inspected first; cloning and polling its receipt still happens only when present, exactly once at this location. The successful receipt, disconnected receipt, retained causes, and later owner-lifecycle checks are unchanged.
2. `runtime.rs:2067` and `runtime.rs:2227`: collapse the nested watch-error and absent-output conditions into let chains, retaining that order. A watch check is not moved behind the output check. A previously selected output is retained, and both callback/failure destruction catches remain in their original positions. An unused owned error is dropped before returning the completion, as before.
3. `vm.rs:2397` and `vm.rs:3190`: give each immediately invoked worker closure a local name and call it at the same position. The Host closure is mutable because it mutably borrows its captured child/backend and executor when called. The Tool closure consumes Tool state, so it needs no mutable binding. Both remain `move` closures with the same bodies and capture order. The named closure is declared after `_completion_notice`, so its captured owners are destroyed before that notice on ordinary return or unwind; the Tool closure's consumed captures remain in the invocation. No worker code was moved across the panic-catching or driver-completion boundaries. Removing these closures outright would require re-establishing the captured-owner destruction scope; this change deliberately retains it.
4. `vm.rs:2882`: return the fork owner's final match directly instead of binding and immediately returning it. The actual source has no intervening cfg(test) observation here. The surrounding panic-catching closure stays in place, including the `?` from `route_entry_outcome(Ok(()))`; it still returns through that same closure rather than escaping the outer owner. Clear-TID, completion publication, lifecycle callback construction/poll/destruction, typed result mapping, and error arm remain unchanged. The nested call was indented locally by hand; no formatter ran.
5. `vm.rs:4578`: replace `.err().expect(...)` with a `let Err(error) = watch.check() else { panic!(...) }`. `EntryDriverWatch::check` has the concrete return type `Result<()>` at `entry/driver.rs:155`. Its unexpected success payload is therefore exactly `()`, with no destructor or generic Debug requirement. The same check runs once; the same literal panic text is retained, and the same error is returned otherwise. This does not introduce `expect_err`'s Debug bound or append a formatted success value to the diagnostic.
6. `vm.rs:4936`: add a function-local `CaptureFuture<'a>` type alias inside the existing cfg(test) module and use it for the capture future binding. Its expanded type is exactly the old type. Both capture branches, the four event cases, two Pending polls, register/frame comparisons, poison cause, cancellation status, and invalid-boundary checks are untouched.

The diagnostic input is the retained copy `lint-v2-stderr`, authenticated in `INPUTS.json`, from the actual strict Clippy v2 run. The earlier SIGXFSZ attempt is not evidence for these corrections. No lint allow, expectation, skipped test, changed threshold, changed error code, or changed result comparator was added.

## Source checks and limits

`git apply --check --reverse` accepted the complete saved patch against the edited live files, without modifying source. Its command, return status, and streams are retained. `CHECKS.json` also records equal ordered function-name lists, equal ordered assertion-macro-name lists, and equal test-attribute counts for each file. These lexical checks are only consistency checks; they are neither Rust parsing nor execution evidence. Manual inspection of every changed hunk found the assertion arguments unchanged.

The before/after file records and packet records use SHA-256. The source edit was not qualified in this pass; the parent must run the authorized shared compiler/Clippy/test qualification after independent review. In particular, this packet does not claim that naming the closures has already passed the installed Clippy version, or that the new source passed the earlier 231-test selection. It does not assess all repository lints, full Hermit determinism/parity, or landing readiness.

## Goalpost-moving assessment

- Assertions weakened: no. Test assertions are unchanged; the interrupted-entry impossible-success condition and literal panic text remain.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. No tolerance, case selection, or comparator changed, and no suppression was added.
- Failure renamed or relabelled as a pass: no. Existing failing lint evidence is retained. This report does not relabel source inspection as a passing build or test run.
- Check deleted instead of satisfied: no. Each changed conditional still executes its original checks in order. The alias and return-binding removal change spelling only; the closure scopes and the impossible-success failure remain.

This is an author implementation report, not independent approval of these edits.
