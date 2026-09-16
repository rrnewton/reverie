# Frozen typed validation provenance recovery

Exclusive writable slot: /home/newton/work/dev-hermit/worktrees/slots/dev-hermit-ci-provenance-20260915.
Registered native worker identity: dev-hermit-ci-provenance. TaskGraph ownership remains dev-hermit. No TaskGraph mutation, push, PR or landing occurred.
All three branches are dev-hermit/ci-provenance-20260915. Every tracked tree is clean; this HANDOFF.md is intentionally untracked. Independent review is pending.

Provisioning used supported wrkslots create with coordinator authorization, owner/coordinator PID 2162531. The original dev-hermit identity refused because it already owns another slot; the coordinator explicitly assigned this distinct native worker identity. No old slot was modified or rebound.

## Frozen chain

- agent-utils: base `14589e875c08e458d2f8af5a6f78b12128d5a367`, head `a640925d4ae400e8127ad2e1aa6c69c9ec2851c9`, tree `dbb1e0bcfb6f2d9580fbd4446c7a09dba8b19025`.
- hermit: base `29927543dea79fca9cb868c1bcb38f7e015595c3`, head `d9ddfef72e0bdc64f1a393034880a2d189571a7c`, tree `f9b760db6ba8b380cee2d9fd7c006ac4310dec69`.
- parent: base `58b3208e0ee7355e2686a49f94683fb8ff5365a0`, head `c2944524e1b8f981774b22aaef8703f991752098`, tree `e3b94060a0031e186700090537d4c7c193dc0e7a`.

Child gitlinks match these exact heads. Machine-readable identities and complete final binary diffs are retained in ignored/provenance-replay/frozen-chain.json and agent-utils-final.patch, hermit-final.patch, parent-final.patch.

Original chain imported through local Git transport from the actual repositories under /tmp/dev-hermit-validate-status-real-histories-20260914: agent-utils 9f45365404be928e62e1518e5c21c91f8936e79e, Hermit 85ae504ad53da0592bc30f547cd9753a72fab9d1, parent ebd494ab1aec9b0c77a346499ac66f42dcc7c90b. Originals remain untouched. Parent initially provisioned at 2b2490c153e0b7d6a1f75f4e2e7e461f028c5f0f and advanced to fetched 58b3208e before replay; children use the exact fresh bases above.

## Scope and replay adjustments

Schema 6 preserves no_result/refused/interrupted causes and ordered concrete detail through the actual writer, service reader, durable run handles, release output, cleanup and status crosscheck. Existing real-history classification now matches the Rust authority while preserving genuine typed product failures and historical ambiguity.

Safe recovery adjustments beyond original bytes:
- List/dict causes previously raised TypeError in terminal/cleanup predicates. They now refuse explicitly. Retained malformed-kind red/green controls prove the defect.
- Rust/Python current readers collapsed duplicate JSON keys. Schema 6 now parses original bytes strictly, including direct raw-handle reads in run_registry, signal_crosscheck and start_unit cleanup discovery. Agent-utils already had strict raw JSON reading; tests exercise it directly.
- Schema 1 through 5 compatibility remains unchanged. Existing explicit-null verification-report behavior is tested, not redefined.
- Current lifecycle populations were 653 = 98 ordinary + 555 mapped. Twelve additional parametrized cases produce 665 = 98 ordinary + 567 mapped; the exact collection assertion was updated and passed.
- Child gitlinks point to actual replayed commits. No other gitlink changed.

No null canonical-reason policy change, publication-dispatch fix, scorecard caller/storage migration, count overflow fix or observer-liveness repair is included. The separate observer patch only overlaps run_registry.update_from_observer; this chain does not touch that function or pane_watch.py production code. Recovered pane_watch test changes are schema fixture fields only.

The committed DAG, exact Rust-script producer guard and producer function are byte-identical to Hermit base 29927543, including the 6 GiB memory bound (producer-preservation.json). No tolerance, gate, comparator or test population was weakened.

## Fresh checks and retained evidence

All evidence is under ignored/provenance-replay. Each run_check.py invocation has a .log and .json recording argv, cwd, return code, elapsed time and deadline. Rust commands used offline Cargo with two jobs and private slot build/cache directories. Ordinary ephemeral /tmp fixtures were explicitly authorized by the coordinator; source and retained evidence stayed in this slot.

- agent-utils-schema-and-partitions: 20 passed, including strict current and historical projection and exact collected partition controls.
- agent-utils-contracts-corrected: strict mypy passed 318 files; no-Any guard passed 319 files; dependency, test selector, validation selector, client-name and documented-default contracts passed in 10.68 seconds. First run caught a missing annotation in a new malformed-input test; corrected with tuple[object, ...].
- agent-utils-lifecycle-ordinary: 98 passed, 567 deselected, 261.10 seconds.
- hermit-service-result-native: 12 native tests passed, 44.64 seconds including compilation. Command: cargo test --offline --locked -p hermit-manifest-plan --lib service_result::tests -- --nocapture.
- parent-rust-writer: actual Rust writer -> Python reader, 1 test and 8 subtests passed in 131.73 seconds. All three causes/detail and malformed evidence exercised, plus existing explicit-null verification compatibility.
- hermit-writer-unit-tests-filtered: 3 passed, 46.05 seconds including compilation. Command: rust-script --test scripts/validate.rs final_validate_status_tests -- --nocapture. Earlier CLI attempts rejected incompatible --force/--test and missing Cargo argument separator; no tests ran in those attempts.
- parent-real-history-parity: 1 test / 3 retained events passed in 76.98 seconds. Events legacy-000347-000, legacy-000519-000 and devbig014-1788131803-1801367 were compared to the actual Rust validate-status output, not a Python imitation.
- Parent eight-file focused run originally passed 373 tests; 90 failures were 87 methods plus 3 subtests caused by an overlong isolated TMPDIR (130-byte socket versus unchanged 30-byte production bound). Only failing methods/subtests reran after switching to ordinary /tmp: 87 methods passed in 18.20 seconds; 1 method / 3 subtests passed in 0.48 seconds. No production bound changed.
- parent-crosscheck-cleanup-final: 307 surrounding crosscheck/startup tests plus 60 subtests passed in 31.12 seconds after fixing their raw-reader bypasses.
- parent-duplicate-legacy-boundaries: 2 tests / 16 subtests passed in 0.49 seconds, including duplicate current cause/detail/schema markers and unchanged historical schemas 1 through 5.
- rust-duplicate-control-corrected and python-duplicate-and-legacy-control.json compare actual original recovered readers against current readers: three duplicate forms accepted before and refused after; valid schema 1 through 6 bytes remain accepted by both. Original raw modules are retained in duplicate-control/.
- parent-duplicate-bypass-red: six failing raw-reader subtests before the crosscheck/cleanup fixes. First green attempt exposed a test typo naming unreadable instead of unreadable_handles; the assertion was corrected to the real report field and all controls passed.
- rustfmt check on service_result.rs passed (format-check.json); all three git diffs passed whitespace checks.

## Incomplete coverage and publication requirements

The broader AU mapped lifecycle partition reached its original 300-second orchestration deadline after 179 progress dots with no assertion failure. It DID NOT finish. SIGTERM then SIGKILL ended the owned group; lifecycle-process-group-readback.json verifies both mapped and ordinary groups empty. No broad mapped rerun was attempted. Focused schema controls and ordinary partition are green, but full canonical agent-utils make validate remains required before any push.

No full Hermit validation, backend guest execution or qualifying validation receipt is claimed. Focused checks ran against the final source bytes before commits; the commits add no later source edit. Supplemental receipt suite reached its original 300-second deadline after 32 result markers, including one failure. Only the observed failing case and the unfinished cases were rerun, followed by the not-yet-run failure-evidence/gate-completeness files: 74 tests and 14 subtests passed, two receipt tests failed in 49.83 seconds (parent-receipt-targeted-completion). Both exact failures reproduce on pristine source bases parent 58b3208e / Hermit 29927543 / AU 14589e87 in isolated local fixture clones under ignored/provenance-replay/base-control: 2 failed in 87.37 seconds (parent-receipt-base-control). receipt-base-failure-comparison.json verifies their assertion diagnostics are identical.

The two existing failures are test_live_refuses_semantic_green_from_unregistered_producer_per_leg (six consumer legs accept the unregistered-producer mutation) and test_validate_status_cli_keeps_newer_shape_readable_without_authority (exit 2: cell_results counts or artifact identity malformed; expected 3). No assertion or product code was changed to clear them. The coordinator reported a separate publication-policy change on parent main e5fe1aa8; this frozen source does not duplicate or assume that change fixes these tests.

Parent's bare commit was refused by the shared-index guard; the same explicit owned pathspec committed successfully without override. The hook warned that this deliberate branch slot is not a fresh main checkout, then passed internal pin coherence and attribution. Parent landing must happen later from a clean detached checkout at then-current main, using verified push after review and proper validation.

Exact who-am-i disclosure used in the first commit-body line:
[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]
The unresolved agent field is the actual resolver output, not a claimed resolved identity.

## Changed paths

agent-utils:

- py/tests/test_packaging_infrastructure.py
- py/wrkslots/cli.py
- py/wrkslots/tests/test_lifecycle.py

hermit:

- agent-utils
- ci/manifest-plan/src/service_result.rs
- ci/manifest-plan/validation-service-result-schema.json
- scripts/validate.rs

parent:

- ci-hub/landing/test_pr_head_ci.py
- ci-hub/validate/flake_class.py
- ci-hub/validate/run_registry.py
- ci-hub/validate/service_result.py
- ci-hub/validate/signal_crosscheck.py
- ci-hub/validate/start_unit.py
- ci-hub/validate/tests/test_flake_class.py
- ci-hub/validate/tests/test_pane_watch.py
- ci-hub/validate/tests/test_run_registry.py
- ci-hub/validate/tests/test_service_result.py
- ci-hub/validate/tests/test_signal_crosscheck.py
- ci-hub/validate/tests/test_start_unit.py
- hermit
- scripts/release-0.3-acceptance.py
- scripts/test_release_0_3_acceptance.py

