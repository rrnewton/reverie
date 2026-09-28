# M2 v7: current typed-report consumers

V7 prepares the three remaining donor-backed consumers identified for
https://github.com/rrnewton/hermit/pull/2302. It uses the existing
`verification-report canonical-match` command and current `VerificationReport`
parser. No old handwritten acceptance schema, success-banner fallback, or
output-only comparison is introduced. This is an isolated source preview
against Hermit `8051335e87104f7cf832204f74920d38416393b2`, tree
`43d26909dc39a4e112b4e3f77254666c86fe548a`. No live product source, index, refs,
skills, protocols, claims, or public branches changed. No build, test, guest,
package-resolution, or network operation ran.

`candidate-v7.patch` is the complete 29-path patch against that immutable base.
`v6-to-v7.patch` contains this successor's eight changed paths. Its separate
`preview-v7/` materializes 35 files, including the six unchanged surrounding
files carried from v6. `PREVIEW-MANIFEST-v7.json` binds every file and its base
blob; `BASE-v7.json` explicitly records the new helper's absence at base. All
v5/v6 packet inputs and preview bytes remain unchanged.

## Consumer behavior

`scripts/manifest-to-commands.rs` passes each already-rendered command to the
small `scripts/lib/verified_command.rs` formatter. Verify and replay retain the
existing `verify.json` destination; chaos retains its seed-specific destination.
The resulting subshell checks that the selected reader exists, removes only
that attempt's prior report, runs the unchanged producer command, and invokes
the current typed reader. It returns the producer's status only after the
reader succeeds. A valid report can therefore preserve guest exit 23; a missing,
empty, malformed, contradictory, or noncanonical report cannot return 23 as an
accepted verification. Custom commands remain unchanged. The caller's original
Hermit arguments, environment strings, timeout, remaining-time calculation,
repeat count, backend/seed selection, and guest argument handling are unchanged.

The formatter is a pure Rust string function, not a new executable protocol.
Both the real generator and the existing Hermit native consumer target import
the same source. This lets controls invoke the production shell wrapper with
the real compiled reader without making a standalone rust-script test build or
locate Hermit implicitly. The three mode branches all use this production
formatter; a source assertion retains that call site in the native control.

`tests/e2e/lib/applications/common.sh` keeps its established raw-field reads
solely for its refusal, malformed/no-result, infrastructure-cause, zero-count,
and divergence diagnostics. Those branches can reject evidence; they cannot
accept it. Positive acceptance additionally requires the current typed
`canonical-match` reader. This preserves the existing diagnostics while
rejecting false `compare_logs`, Stripped policy with a true parity claim,
contradictory `verified`/verdict fields, incomplete reports, and unequal counts.
The absolute-argument contract, ordinary private `/tmp`, pinned `/test` mount
and workdir, producer timeout, captured output, cleanup, and post-verdict guest
status handling are unchanged. Missing readers refuse before guest launch.

`tests/reproducible-builds/run.sh` requests `--verify-json`, removes the old
report before the invocation, validates it through the typed reader, and only
then handles the compiler guest's status. It retains the report beside the
objects. Its two native builds must still differ, and its two independent
strict Hermit builds must still produce identical objects. The compiler argv,
proc-macro prebuild, fixed fixture workdir, output unlink during each verify
run, object comparisons, and artifact-success message are unchanged. The
README now names the reader build/override and the actual report requirement.

## Prepared discrimination controls

The existing `hermit-cli/tests/verification_report_consumers.rs` target retains
all prior test functions and adds three native methods:

- `generated_verification_commands_validate_fresh_evidence_before_guest_status`
  executes the shared production formatter for verify, replay, and chaos with
  a synthetic producer and the actual reader. It brackets success and guest
  status 23 against absent/empty reports, an invalid JSON type, missing current
  fields, contradictory policy/log/verified fields, zero and unequal counts.
  Each attempt begins with an old valid report to expose stale-file acceptance.
  It also checks the seed-specific path, sibling reader resolution, an explicit
  unavailable-reader refusal before producer entry, and unchanged custom mode.
- `application_shell_consumer_uses_the_real_typed_reader` runs the actual
  `test_verdict_discrimination.sh` through the compiled reader. The original
  positive and every original negative/path control remain. Its positive
  fixture now supplies the complete current producer fields and both output
  operands; the old incomplete positive could not qualify through the current
  reader. Added controls cover guest status 23, contradictory fields, an empty
  file, and the unchanged infrastructure-kind/count diagnostic. Negative
  assertions now also reject an escaped status 23. Existing `/test` and
  absolute-path assertions are unchanged.
- `reproducible_build_consumer_keeps_artifact_and_typed_verdict_requirements`
  executes an exact copy of the production runner with controlled command
  outputs and the actual reader. It requires both independent object oracles,
  then brackets the report gate with valid, guest-23, contradictory, absent,
  and empty cases. Equal native objects and different Hermit objects must fail
  before verification entry. These controlled objects do not represent real
  compiler, proc-macro, Hermit guest, or VM execution.

The existing named-consumer table adds applications and reproducible builds;
its source declaration changes from 11 to 13 entries while preserving every
old entry and its requirement. These are source declarations, not measured
test inventories or execution counts. No selector, ignore marker, tolerance,
existing assertion, or generated count was weakened or removed. Current
`canonical_verdict.rs` and every historical report reader remain byte-identical
to v6/base. The earlier core numeric/UTF-8/INFO and wrapper assertions remain
unchanged in the complete patch.

## Reader packaging and exact limits

`hermit-cli/src/bin/verification-report.rs` is an automatically discovered
binary in the existing `hermit` Cargo package; no Cargo dependency, feature, or
manifest change is needed. Its current parser requires producer fields and
exact output operands, and its canonical-match requirement checks the policy
and equal positive INFO counts. The new native controls reuse the existing
`CARGO_BIN_EXE_verification-report` binding. Standalone shell callers use the
same `VERIFICATION_REPORT_BIN` override and sibling default already used by
the data-handling, stress, and language-runtime consumers.

There is a concrete packaging distinction to preserve when composing the
eventual commands. `cargo build -p hermit --bin hermit` alone does not build
the reader. The reproducible-build instructions now request both binaries;
the generated-command help and all new preflights explain the reader build or
explicit override. Existing portable debug artifact packing/unpacking and
`ci/check-shard-coverage.sh` already include/check `target/debug/verification-report`.

In contrast, the separate `ci/publish-hermit-e2e-artifact.sh` bundle publishes
`hermit` and optional install resources, not a sibling reader.
`ci/run-with-hermit-e2e-artifact.sh` exports that bundle's `HERMIT_BIN` but does
not set `VERIFICATION_REPORT_BIN`. A consumer invoked through that maintained
bundle path must therefore explicitly bind `VERIFICATION_REPORT_BIN` to an
already-built current reader; otherwise this preview correctly refuses. No
artifact protocol or implicit fallback was added. The eventual bounded caller
must include and bind this helper before claiming the bundle route runnable.
That is a concrete remaining composition input, not a demand for a full-DAG
receipt or unrelated packaging redesign.

`inputs-v7/SOURCE-INPUTS-COMPLETE.json` binds the immutable reader, schema,
Cargo declarations, existing consumers, and packaging sources. An early lookup
of `tests/e2e/lib/qemu/strict-boot.sh` was absent at this base; the separately
bound existing `tests/qemu-boot/strict_l2_test.sh` is only corroborating source,
not a substituted invocation. The added Rust source module must join the
normal source/dependency closure when preparing a generated-script artifact.
The existing `rust-script --force` path remains unchanged.

## Corrected v6 matrix disclosure

V6's backend-parity README correction removed too much of the unresolved
current-matrix note. V7 restores the exact active obligation: unchanged
`run_matrix.py` still sets `DEFAULT_VERIFY_POLICY.expected_non_kvm_tier` to
`stripped` around line 261, KVM's expectation to `guest` around line 716, and
the printed KVM ratchet to `guest` around line 2002. Its checked policy also
couples the strict spelling to the bitwise expectation. The current typed
parser may produce a `bitwise` result, which satisfies those weaker expected
tiers, while the requested-policy/ratchet descriptions stay stale. These are
active policy/label consumers, not merely historical decoding. No matrix code
or discrimination test was changed here. Root owns their separate disposition;
this preview does not claim the donor closed or the matrix remeasured.

## Readback and remaining execution

`INTEGRITY-v7.json` records exact in-memory application of the complete and
incremental patches, including the new file. All outputs match their manifests.
It separately compares unchanged generated producer argv and timeout/repeat
logic, the full native/Hermit object-comparison section, application argument
and execution-root handling, and all previous native consumer function names.
Every v6 Rust/demo byte remains unchanged. Only the two declared README paths
change among the v6 files. `format-v7/RESULT.json` retains the standalone
nightly rustfmt outcomes for the three new/modified Rust files under the same
10 CPU-second/20 wall-second limits; the generator subsequently gained only
its three-line usage comment. Formatting is not compilation or execution.
`PREPARATION-v7-refusal.json` retains the harmless failed append whose relative
owned destination was absent; the correction used the full owned path.

The next owner still needs exact composed-source compilation and actual
inventories, the existing bounded native consumer target with its real reader,
the generator's existing tests, and appropriate original producer/application/
reproducible-build guest qualification. The core M2 obligations and known
trace/chaos coverage gap remain in `REPORT-v5.md` and the separate validation
map. No new validation protocol or larger resource budget is proposed here.
No producer or consumer execution result is claimed. Incoming current
integration changes to `hermit-cli/src/lib.rs` remain separately owned; that
file is still an unchanged base copy outside this patch. Parent review and
composition after the current linear landing remain required before live
application or publication.
