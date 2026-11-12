Review target: completed evidence for Hermit 5bc8dfd9a2c024426aaefe504f250a71394bbb09 (tree d828fca5783a8183e1cff1d42937a2beb5d4f743), using landed Reverie 7d863ab3f02639731713a01467b2548c41e3dbfb. This is result authentication, not another source approval or a full-backend verdict. Owner evidence is under kvm-prejoin-main-20260917/ignored/prejoin-main-callers-v11 (C11). This report complements the separately completed original 24 CLI review in the parent directory.

Findings and disposition:

- No remaining concrete blocker in the scoped pthread execution evidence. The original lifecycle method, its mandatory production typed writer, and both per-backend canonical repeat comparisons and typed readers qualify. No guest was rerun to repair a reader refusal.
- Preserve KVM's original accepted=false record: guest-controls-run-3/pthread-canonical-kvm/result.json SHA 9e51c54a4f2a35c84d8c43e62d2339d474770b2675273f15faf175d9571b1bae. Its actual payload returned 0, but the final five-second git rev-parse HEAD query was killed with -15 after 5.211451529 seconds, with empty stdout/stderr. This is a source-query timeout, not a failed guest or failed canonical comparison. The original result and sequence exit remain unchanged.
- The separate read-only kvm-canonical-readback-v1/result.json SHA f3e447acab832862a5cc82019c8316ec6f3d0115944d4948bdcb9e8bebb44870 rechecks the original exact plan, source, inputs, SCM, raw report/log interpretation and inactive/empty service. It preserves exact report equality and directly requires positive equal counts. The final previously unexecuted typed reader then ran once and passed; no guest reexecution occurred.
- Reporting correction sent to root/owner: FINAL-GUEST-REPORT.md paragraph 2 overstates the original helper's stderr assertion. kvm_harder.rs checks successful exit and exact stdout, then successful default verification and the Success banner. Empty guest stderr is proved by the separate canonical reports. This is a prose correction, with no change to execution acceptance.

Original method and producer shape:

Exactly one selected, nonignored method ran once: hermit::kvm_harder$kvm_matches_ptrace_for_pthread_lifecycle. Actual inventory has 499 entries and selects only that method. Its unchanged source always compiles pthread_lifecycle.c with cc -std=c11 -O2 -g -Wall -Wextra -Werror -pthread, asserts compilation success, and then runs ordinary strict and default --verify arms on ptrace and KVM. It checks status 0 and stdout `threads=4 total=10\n`; it does not perform cross-backend INFO comparison or set /test for the original method. ORIGINAL-FIXTURE-IDENTITY.json verifies the helper and C fixture are unchanged from immutable 805 through 5bc.

The raw seven-event stream has one started/ok pthread identity and its successful suite recap, followed by a zero-executed CLI suite containing only a started DBT identity and a recap passed=0, failed=0, ignored=1, filtered_out=111. The actual inventory marks that exact DBT identity ignored=true and filter mismatch reason=ignored. This is not an additional guest execution or a selected ignored test. ci/nextest-test-results.rs:253–398 reconciles closed typed suites, skips started/ignored rows for executed results, checks terminal execution totals and reconciles every real CPU attempt. Its existing repeated_suite_recaps_do_not_multiply_filtered_count control expressly models started-only ignored output. The CLI-only addendum's explicit terminal-pair rule is not imposed on this different producer shape.

The production writer's schema-2 output has exactly one pass, attempts=1, and filtered_tests=115 (harder 3 plus CLI 111 filtered and 1 ignored); 115 is not the global 498 unselected inventory. The schema-3 record has exactly one matching wait4-subtree attempt, exit 0, CPU 542739 microseconds, wall 771 milliseconds. Its wait4 user 397894 plus system 144845 microseconds equals the recorded CPU, and the raw file equals the typed attempt. All seven identical missing build-script-output warnings are retained; they are not silently dropped.

Artifact and comparison scope:

- Actual test ELF: kvm_harder-624fd08e2e974914, 7,919,920 bytes, SHA 7d0ce6863fc692df93af7fc601617b8e95ec1a4c7c1afbba8b6036a9a64c9835.
- Actual Hermit ELF: 411,317,048 bytes, SHA 5a16cb09205ded3f05bc3bfb9db32ef6a1c90c408e1068cf3b10fc32639ffd0d.
- Actual produced guest: target/tmp/kvm-harder/pthread_lifecycle in the owner's retained build target, 20,816 bytes, mode 0755, SHA d12d7e69e764aa0c351c1ee694d754b88ca1eec3e86b130b25ebd86dcd3947b5. The successful original method's unconditional compiler/assertion path establishes production; later plans bind this exact artifact. There is no separate pre-run absence assertion claimed. Both explicit backend phases consume these same guest bytes without recompilation.

Both explicit phases use the original --log=info, --strict --verify --verify-strict, minimal environment, fresh tmpfs at /test, and --workdir=/test argv. Both complete ComparisonSpec objects equal the original frozen strict_policy: BitwiseInfoV1, canonical, all_records_v1, INFO scope, I/O comparison, exact remainder, address ordinalization and only the named wall-clock prefix treatment, with strip/ignore/skip switches false. Ptrace compares exactly 229/229 INFO records; KVM exactly 219/219. Counts are explicitly equal within each backend, not inferred from verdict. Both sides have exit 0, no signal, identical 19-byte stdout SHA 6f916c392b593c2835823f98adada81a14b81ff8683b4045610a9a1cc0728563 and empty stderr. Both per-backend runs report 19 scheduler turns and 81 syscalls with equal virtual time within that backend.

All four full log files are hashed and retained: ptrace 53,650 bytes each, KVM 50,565 bytes each. They contain respectively 281 and 271 physical lines because COMMIT records span lines; the INFO record starts are exactly 229 and 219. detcore/src/logdiff.rs:614–713 defines that multiline record boundary. No continuation lines were deleted or filtered. Each current producer-owned verification-report --json canonical-match reader exits 0 and emits the complete same parsed report. These are self-repeats within ptrace and within KVM. No cross-backend canonical INFO parity is asserted; 229 and 219 are different populations.

Actual service accounting (CPU seconds / observer wall seconds, not only guest duration):

| Phase | CPU seconds | Wall seconds |
| --- | ---: | ---: |
| original-pthread-inventory | 0.451256000 | 0.864983535 |
| original-pthread-binary-map | 0.702186000 | 1.671825729 |
| original-pthread | 3.095280000 | 3.231643165 |
| original-pthread-typed-results | 0.206794000 | 0.702355296 |
| pthread-canonical-ptrace | 0.851932000 | 1.355883490 |
| pthread-canonical-ptrace-typed-read | 0.557055000 | 1.038992728 |
| pthread-canonical-kvm | 1.124671000 | 1.796497242 |
| pthread-canonical-kvm-typed-read | 0.559300000 | 1.073946356 |

Every execution retains exact admitted argv/environment, bound ELF/loader/helper inputs, service PID/start ticks/cgroup identity, complete aggregate accounting and inactive/empty terminal status. The independent readers issued two fresh terminal queries per each of eight execution services, and authenticated the recovery's separate queries. Actual API 12 admission preceded the original method and explicit KVM phase. Original pthread/canonical guest bounds remain 30 CPU/60 wall seconds; canonical typed readers 5 CPU/15 wall; all services 16 GiB and zero swap, with existing 16 MiB stderr/postread and 64 MiB observer-directory guards. No claim covers arbitrary guest-written files outside those guards.

Goalpost-moving assessment:

- Assertions weakened: no. Original helper/C fixture bytes and exact selected status/output assertions survive. Both canonical outcomes meet the original full policy and explicitly equal nonzero counts.
- Tolerance widened, exemption added, case skipped, comparator relaxed: no. The ignored DBT metadata is derived from the actual unchanged inventory and a zero-executed typed suite, not selected-test exclusion. No retries or deadline changes were used for these guest executions.
- Failure renamed as pass: no. The KVM source-query timeout remains an original refusal. Its separate read-only recovery is clearly labeled; guest comparison success and later completed source authentication are distinct facts.
- Check deleted instead of satisfied: no. Full source/input/SCM/terminal checks were completed separately, and the original required typed reader executed once.

Verification and preserved local review limitations:

The core independent readback has 440,870 checks, 30,234 unique hashed files and all 1,732 Git source blobs/modes; the final recovery/reader readback has 95,158 checks with fresh complete source checks. Both passed. Review used file hashing, JSON parsing and bounded read-only Git/systemctl queries only; no product, guest, test or owner reader execution was performed by this reviewer. Earlier own evidence-reader attempts remain: a 16 MiB whole-text read refused the large cells.json (corrected to streaming blob hashing), a mistaken physical-line count rejected valid multiline INFO records (corrected to the existing production record semantics while preserving all bytes), and the companion reader initially treated a terminal-query summary list as one query and guessed --require instead of the actual maintained --json canonical-match syntax. All four own reader errors are preserved in their original outputs; none is a product failure or a reason to change product assertions.

Verdict: scoped evidence accepted for the original pthread method, production typed result writer, and separate canonical per-backend repeats/readers on exact 5bc. The separate original 24 CLI review remains in the parent REPORT.md. No whole-backend, full-suite, cross-backend INFO or new source approval is implied.
