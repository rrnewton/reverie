# M2 v6 isolated preview

V6 corrects seven ordinary README files and the analyze comment and displayed
reproduction command. All comparison logic, tests, fixtures, assertions, and
demo executable bytes are identical to v5. This is source preparation against
immutable Hermit `8051335e87104f7cf832204f74920d38416393b2`, tree
`43d26909dc39a4e112b4e3f77254666c86fe548a`; no product source, index, refs,
skills, or protocol was changed, and no build, test, guest, or network operation
was run.

`candidate-v6.patch` is the complete 23-file change against that base.
`v5-to-v6.patch` isolates this successor's eight changed paths. The separately
owned `preview-v6/` materializes 29 files, including six unchanged surrounding
files. `PREVIEW-MANIFEST-v6.json` binds their exact paths, modes, source hashes,
and immutable base blobs. The original `preview/` and every input in
`PACKET-v5.json` remain byte-identical; v5 was not modified to present v6.

## Exact successor changes

- `README.md`: describe canonical INFO as the verification default, retain
  the harmless strict compatibility spelling and typed nonempty-report
  requirement, and identify old SaBRe Stripped measurements as historical.
- `demos/README.md`: explain why the existing demo's strict spelling remains
  compatible with the new default. Keep its actual command, typed verdict,
  guest markers, clock checks, and all measured numbers unchanged. The
  incomplete old PMU attempt remains an incomplete historical attempt.
- `ci/compat-envelope/README.md`: describe the current default without
  converting same-backend comparisons into cross-backend parity or selected
  rows into measured passes.
- `tests/backend-parity/README.md`: distinguish the recorded Stripped/guest
  tables from current canonical invocation instructions. Historical report
  tier decoding remains relevant; no historical row was remeasured or
  relabelled. The existing current typed reader remains unchanged.
- `tests/e2e/determinism-stress/README.md`: describe the new default while
  retaining the actual marker-based acceptance limitation and the distinction
  between repetition counts and qualified evidence.
- `tests/reproducible-builds/README.md`: preserve the native-different and
  Hermit-equal artifact claim and explicitly identify the missing typed-report
  consumer. Canonical defaults alone do not discharge that obligation.
- `tests/bin/README.md`: identify the old polling-mode Stripped result as
  historical; retain its command/output and the robust-futex source narrative.
- `hermit-cli/src/bin/hermit/analyze/phases.rs`: replace the stale “weaker”
  description and remove rejected `--ignore-lines=CHAOSRAND` from the printed
  reproduction command. It now displays `--canonical-info` and the existing
  `--record-envelope=all-records-v1`. The actual `LogDiffCLIOpts::new` call and
  every comparison/control statement remain byte-identical to v5. This
  resolves the sole concrete finding in the independent v5 source review.

The complete v5 implementation and its behavior/limits remain described by
`REPORT-v5.md`. In particular, strict run/record defaults, lossless numeric and
INFO comparison, invalid UTF-8 refusal, active lossy-option removal, strict
wrapper command construction, exact COMMIT comparisons, all-record dispatch,
typed refusal/report retention, and historical report decoding are unchanged
by v6. The v5 analyze migration still deliberately compares CHAOSRAND/SCHEDRAND
records; this successor does not add a filter or assert unchanged analysis
outcomes.

## Donor obligations still open

This preview does not close
https://github.com/rrnewton/hermit/pull/2302. The source-backed donor disposition
is retained in adjacent `../m2-donor-scope-20260917/REPORT.md`, SHA-256
`bfdf559d35c8dffdf32504e0f520535edda0547accba7402f192d8bfdf2207f6`.
Its three remaining concrete typed-verdict consumers are:

1. `scripts/manifest-to-commands.rs`: generated verify/replay/chaos commands
   must validate current canonical evidence before preserving a successful
   verification's nonzero guest status, including guest exit 23.
2. `tests/e2e/lib/applications/common.sh`: retain infrastructure/no-result
   diagnostics and require canonical strictness with log comparison enabled;
   contradictory report fields must not pass.
3. `tests/reproducible-builds/run.sh`: retain the independent object-file
   comparisons and add current typed nonempty-report consumption for its
   verification invocation.

The parent authorized a subsequent isolated preview for these consumers. No
consumer source change is included in v6. Other non-README historical/current
wording noted by the donor report is outside this successor's narrow scope.
Protected stale skill guidance remains recorded in
`PROTECTED-DOCUMENTATION-v5.json`; no skill was edited or made a new execution
gate.

## Readback and remaining validation

`INTEGRITY-v6.json` records strict in-memory application of both the complete
patch and the v5 increment against exact Git/predecessor bytes. All 29 outputs
match their manifest. Every v5 packet input and preview file was rehashed.
The only Rust delta is exactly the two declared prose replacements; all test
bytes and demo executable bytes remain unchanged. A preparation text-match
assertion refused before packet creation; `PREPARATION-v6-refusal.json` retains
that non-product failure and its original caller. No failed product run was
replaced or reclassified.

Affected source test names and all v5 execution obligations are unchanged in
`AFFECTED-SOURCE-TEST-NAMES-v5.json` and the separate
`../m2-validation-preparation-20260917/REPORT.md`. Those are source selections,
not emitted test inventories. No count or generated graph was updated. The
eventual composed source still needs its actual compile/inventory, existing
bounded native and guest paths, original run/record/analyze and wrapper
controls, current source reviews, and normal linear landing. The recorded
trace/chaos coverage gap remains explicit; a full-DAG receipt is not made a
prerequisite. There is no textual overlap with the current integration's
`hermit-cli/src/lib.rs` failure-cleanup edits: that file remains an unchanged
base copy in this preview and is absent from the patch.
