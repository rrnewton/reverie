Exact qualification is recorded in RESULTS.json, with each plan, actual outcome, raw stdout/stderr, source continuity and terminal accounting. All 66 corrected phases qualified; metadata is included in that count. Each test declaration ran once, exact selected count1, no ignored/measured/failed tests, and REVERIE_REQUIRE_KVM=1. Current component inventories:495 lib and304 static; selected50 and9 respectively. No discovered-but-unexecuted case is a pass.

| Phase group | Phases | Aggregate CPU seconds | Sum of observed wall seconds |
|---|---:|---:|---:|
| Compile | 1 | 15.871 | 7.746 |
| Format | 1 | 2.689 | 3.241 |
| Lists | 2 | 4.363 | 5.381 |
| New unit controls | 7 | 15.065 | 18.705 |
| Unchanged library controls | 43 | 93.157 | 116.625 |
| Unchanged VM controls | 9 | 21.603 | 31.099 |
| Core + ptrace | 1 | 2.551 | 3.070 |
| Clippy | 1 | 7.094 | 7.903 |

Corrected phase total including metadata: 163.610 aggregate CPU seconds and 195.524 summed observer wall seconds. These include source/accounting verification overhead; actual harness execution times and payload times remain separately retained in each result. Original unqualified compile: 50.609 CPU seconds and 25.321 observer wall seconds. Its preceding metadata passed; no original-source tests ran. Across both preparations there were68 actual phase attempts,67 qualified and one diagnostic refusal.

The nine actual VM declaration passes retain their original internal loops. Source-derived executions are separate from Rust counts: parked modes0..14 plus16,17,18,19 (19 KVM instances); sibling ten modes in plain and Tool configurations (20 KVM and ten native references); fork/thread exec lifetime two configurations each (four KVM); one signalfd KVM/native pair with its unchanged internal five-shape/thirteen-flag comparisons. Thus the completed test bodies contain44 KVM instances and11 native references. This is a source-derived breakdown of the passing declarations, not44 separately observed receipts or a wider population claim. Existing30-second inner child deadlines are unchanged.

Limits exactly per approved phase: metadata/compile/core/Clippy600 aggregate CPU seconds/900 wall; format/lists/each exact declaration30/60. All16GiB memory,zero swap,16MiBstderr,64MiBmaintainedstdout,16MiBreadback and100GiBfreefloor. Pinned nightly2026-07-29; offline/locked Cargo; two Cargo/third-party jobs. Original observer137c9b42 and lease protocol unchanged. Fresh metadata resolves288 workspace packages; KVM closure155packages with7,083 freshly hashed external files/170,540,804 bytes/zero dependency symlinks. All source/SCM/helper identities are checked before and after each phase. Only the owned target was reused, no foreign cache copied.

Qualified ELF hashes: lib d099437c50c217b1ae61af2278c6350a4cf96740d4169e47feefe277575ba3b4; static90eff37c7e030272481c0ef4c7406e36f88320f4b2a3a90e5981d00a22bf160f. Immutable copies are in qualification-v2/retained-binaries. Compiler JSON, exact target/kind/profile, retained-copy identities, ELF loader queries, actual list output and each exact test result are all bound. The cache filenames may later be reused; the historical record is not a claim those mutable paths retain old bytes forever.

All phase-owned lease descriptors are closed. LEASE-RELEASE.json records a successful nonblocking exclusive availability check, immediately released without state mutation. Product/HEAD/index remain fixed for independent review. No source, compiler or test failure is hidden by that lifecycle check.
