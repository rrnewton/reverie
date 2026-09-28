This isolated continuation changes the seven authorized shell reader requirements from `matched` to `canonical-match`, updates only their seven entries in the retained thirteen-consumer table, and adds one shared source-site/actual-reader control. The system-utilities KVM success line now describes the canonical INFO plus output/exit match required by the changed gate. No live repository or test execution changed.

`candidate.patch` is the complete eight-path increment. The seven shell bases are exact immutable Hermit 14d63ed54b7284b7f8bc29d44c7610a809815621 copies (tree d828fca5783a8183e1cff1d42937a2beb5d4f743). The consumer-test base is the exact immutable M2 preview-v10 file, whose blob is 6754b8336f3cc72175fbdad079a02877d4191340. This is a patch for the continuation after M2 composition, not a standalone patch claiming to turn unmodified 14d into the whole M2 candidate. Apply the hunks to the owner's composed source; do not replace its complete current test file with these older surrounding bytes if it has gained another disjoint change.

PREVIEW-MANIFEST.json binds all eight before/after files, full blob/SHA256 identities and unchanged modes. base/ retains the exact operands; preview/ contains only the eight proposed outputs. The complete unified hunks were applied to the base strings in memory and required to equal every preview byte. Each shell change was reversed and required to reproduce its entire original file. The test preparation likewise reversed the seven table edits and removed only the new method, requiring complete byte identity to M2 v10. No source validation is being reported as compilation or execution.

The changed shell paths are:

- tests/e2e/lib/data-handling/common.bash
- tests/e2e/lib/determinism-stress/common.sh
- tests/e2e/lib/language-runtimes/run.sh
- tests/e2e/lib/system-utils/_common.sh
- tests/qemu-boot/strict_l2_userspace_test.sh
- tests/standalone/strict_setitimer.sh
- tests/standalone/strict_timer_create.sh

Each contains exactly one changed reader argument. All guest argv, statuses, output/category/boot/timer assertions, repeated-run counts, environment/workdir behavior, timeouts, compiler/dependency availability handling and cleanup are unchanged. No extra --verify-strict or timer --log option is introduced: the prepared M2 producer already defaults verification to canonical INFO. Historical comparison-description decoding and the standalone setitimer's documented gap/STRICT_EXPECT_FIRE choice remain untouched. In particular, this does not turn that timer gap or its existing status-swallowing limitation into a functional delivery claim.

The sole additional shell text change is system-utils' KVM success message:

    PASS [%s/kvm]: strict --verify canonical INFO and output/exit match; relaxations=none

That message follows the unchanged producer-status and workload checks plus the newly strengthened reader. It replaces the stale assertion that internal trace comparison is unavailable. It describes the enforced requirement, not a result observed in this preparation. The actual shared reader still has its documented scope: canonical strictness/envelope, log comparison, positive equal counts and current exact-output/outcome checks, not validation of every optional comparison description or proof of cross-backend parity.

`hermit-cli/tests/verification_report_consumers.rs` retains the exact thirteen paths, order, existing minimum invocation counts, the four prior canonical entries and both unchanged Python matched entries. No earlier test method or body was removed or weakened. The only new declared method is:

    remaining_shell_consumers_require_current_canonical_match_evidence

For each of the exact seven shell paths it finds the existing table entry, requires `canonical-match`, requires exactly one matching production call-site string, and rejects a remaining weaker reader invocation. It then invokes the actual `CARGO_BIN_EXE_verification-report` through the existing `verdict` helper. A complete current canonical 123/123 match must exit 0. Stripped strictness, compare_logs=false, zero counts and a 123/124 claimed match must exit 1. Missing report, invalid JSON and absent current output operands must exit 2. The unequal-count case also requires its exact 123/124 diagnostic. Restoring a production site alone to matched breaks the source-site assertion; restoring both the table and source requirement breaks the explicit canonical assertion and the distinguishing negative cases.

This is one prepared native test method, not eight new test identities or measured results. It executes the report reader over synthetic files when eventually run; it does not execute Hermit, a shell workload, a timer or QEMU. The prior historical-report, divergence, infrastructure-error, current-parser and count controls remain unchanged. No shared parser, comparator or schema is modified by this increment; genuine unequal-count divergence remains a distinct accepted comparison and an unmet match requirement.

Execution obligations remain the existing maintained paths. Compose the exact hunks on the actual current M2 candidate, run required source formatting and the affected existing verification-report integration controls, and obtain any changed inventory from the emitted test executable. Preserve all existing target identities and bounds; no generated count is guessed here. The corresponding original shell/guest workloads remain the source of workload evidence where the owner selects affected controls. An actual-reader/source test does not replace their independent oracles or introduce a full-DAG prerequisite.

Packaging is explicitly unchanged. A build of only --bin hermit does not provide the separate verification-report binary. The Hermit-only artifact bundle and its wrapper still do not export a reader. Later consumers must receive the existing VERIFICATION_REPORT_BIN override bound to an actual matching emitted reader, or an appropriately bound sibling through their existing discovery. Missing/nonexecutable-reader refusals remain. No new wrapper protocol or bundle layout is introduced.

Python consumer source and tests were not edited. The matrix's current/historical split and e9patch's report/status obligations remain in the parent REPORT.md; root has separately assigned the e9patch proposal. This patch neither changes their table requirements nor silently implements their remaining semantics. It is an isolated same-continuation proposal for https://github.com/rrnewton/hermit/pull/2302, with no PR, claim, publication, closure or final-source approval.

One preparation-only error is retained in PREPARATION-REFUSAL-v1.json: the first copying script incorrectly asserted that the intended escaped matched token occurred zero times. It refused before writing a consumer-test preview. The successor removes only that redundant incorrect assertion and retains the exact entry/count/reversal checks. Both scripts and the refusal record remain; this was neither a compiler failure nor a product test result.

Goalpost-moving check: no assertion, tolerance, expected tier, workload selection, timeout or comparator was relaxed; no case was skipped or failure relabeled as a pass; no old check was deleted. The new gates require stronger evidence, while the known-gap and packaging limitations remain explicit. No formatter, Cargo invocation, product test, guest, live source/index/ref/cache change or network operation ran.
