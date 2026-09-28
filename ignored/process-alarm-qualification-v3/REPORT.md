[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

The unwired process-pending SIGALRM primitive has completed its focused qualification. Source remains staged and uncommitted on `codex/kvm-setitimer-20260918`; base and HEAD are `99d1e4827cce2404442d7c27ab447886a5839326`. The final eleven-path `SOURCE.patch` SHA256 is `d7dac68d187d99f01bed533435043faac1152a989edd4a814ed6d62c753d1ccc`; `SOURCE_INPUTS.json` lists every changed file. No commit, push or PR was made.

`Guest::queue_process_alarm_signal` and its adapters publish/coalesce canonical complete SIGALRM/SI_KERNEL information in process `shared_pending` after task, transport, lifecycle and single-receiver checks. Typed results separate rejection, acceptance and readiness failure after publication. First information and actual pending domain survive coalescing and Tool reblocking. Historical private deferral remains private. Full behavior and test assertions are described in the frozen original author report at `../process-alarm-primitive-20260918/REPORT.md`; its original statements that qualification had not run describe that earlier freeze.

Actual results are recorded in `RESULTS.json`, with commands, complete logs, plans and input bindings indexed by `EVIDENCE_INDEX.json`:

| Control | Actual result | Observed wall seconds |
| --- | --- | ---: |
| Cold two-artifact compile, original freeze | Passed; zero compiler diagnostics | 58.476 |
| New alarm library controls | 10 passed, 0 failed, 0 ignored | 2.822 |
| Existing child-exit library controls | 9 passed, 0 failed, 0 ignored | 3.009 |
| Existing Tool-domain library controls | 3 passed, 0 failed, 0 ignored | 2.767 |
| Original alarm VM control | **Failed**, raw 101; retained | 2.801 |
| Successor compile after fixture correction | Passed; zero compiler diagnostics | 3.845 |
| Successor alarm VM control | 1 passed, 0 failed, 0 ignored | 3.125 |
| Child-exit VM regression on successor | 1 passed, 0 failed, 0 ignored | 4.401 |
| Focused rustfmt check, ten changed Rust files | Passed; no differences | 3.346 |
| Focused Clippy, library and static_elf target, `-D warnings` | Passed; zero diagnostics | 23.137 |

The successful controls comprise 22 library tests and two VM tests. The alarm VM test contains nine asserted modes; those are not nine libtest passes. Actual inventories contained 464 library tests and 289 static_elf tests, with every selected name present exactly once. Selected controls used `--exact --test-threads=1`; no zero-selected or ignored run qualified. This is focused evidence, not a full-suite pass.

The first VM failure remains in v1 with full stdout/stderr, raw status, terminal accounting and `FIRST_FAILURE.md`. The post-marker libc `getpid()` comparison could preserve the marker in otherwise-unused syscall arguments and enter the Tool marker branch again. Bound libc disassembly supports that diagnosis; no failed-guest register trace is claimed. Root authorized saving the expected PID before the marked syscall and comparing against that saved value. This is the sole successor source delta, frozen as v2 `fixture-delta.patch` SHA256 `b1036079820eff5e5d94d188feda9b99693063ad24db7ea038a9f7433da84ae8`. All assertions are retained. The previously failing exact VM test was retried once and passed.

The successful original library ELF is byte-identical after the correction: SHA256 `4c3293111d45e0225553bd2ba3dac222c64583102cda83eac9c90a09371ad766`. The final static_elf SHA256 is `1f672d39d0a4522a5c2e71009ee8ec4fc4ad9c028c044bba451aa31f6855429e`. Cargo JSON identified both actual executables. v2 `SOURCE_BINARY_BINDING.json` records the predecessor and successor. The old static ELF file was rebuilt in our own target; its original identity and runtime checks remain recorded, but no separate old ELF copy is claimed. Each executed phase checked complete source/index, dependencies, executable and loader identities before and after; terminal empty-cgroup readback succeeded. No resource limit fired.

Qualification used the installed `nightly-2026-07-29` toolchain, authenticated unchanged-manifest Cargo.lock SHA256 `1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856`, `--offline --locked`, two Cargo jobs and an initially empty private target. No foreign target/cache was copied. The 155-package external source closure was bound read-only. Cold compilation consumed 117.102 aggregate CPU seconds; successor compilation 4.946; Clippy 37.493.

The exact approved observer SHA256 `b161c17f850113d5155974d9547ddc842b44c4e37445c44a5d2f06aef500b56e` and unchanged common/lease helpers were retained. Compilation/metadata/Clippy bounds were 600 aggregate CPU/900 wall seconds; inventories/tests/format used 30 CPU/60 wall seconds. All phases retained 16GiB memory, zero swap, 100GiB free floor, 16MiB live stderr, the existing 64MiB live stdout/samples guard and 16MiB post-exit read refusal. The static_elf 30-second self-exec limit was unchanged and `REVERIE_REQUIRE_KVM=1` was mandatory.

Root reported authenticated native approval of the original source freeze (review report SHA256 `21025f97842b6e7e3aa07849e4f76fdb02efd5e6ab83ab41488f34433f3772a1`). Actual Claude review was still pending at this author handoff; both reviews must be reconciled with the isolated fixture correction. This report is author evidence, not independent approval. The disclosure above is exact who-am-i output; its unresolved-identity diagnostic is retained separately.

The Hermit timer failure remains open. No scheduler bridge, periodic rearm, original parked-wait continuation, RPC-origin capability, getitimer interval fix or general multithread recipient selection is supplied. No disabled manifest cell is declared passed. A pending receipt is not dequeue or handler execution. Root owns review reconciliation, commit and verified landing.
