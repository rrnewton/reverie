# Adversarial review: hardened NARF syscall interception

Review target: `/home/newton/temp/agent_checkouts/narf-hermit-poc`, base `87b7cd1048a9492fccb4339558038c06d862e0b4` through exact head `c3a92bbbe38b59649316f5d39ddd0f61fba690ef` (commits `70ed2271` and `c3a92bbb`).

## Findings

- **S1 / blocking — the supposedly test-only reset remains an always-compiled public replacement escape hatch.** `userspace/src/syscall.rs:5410-5417` declares `pub fn __test_clear_global()` with no `cfg(test)` or `cfg(feature = "kernel-test")`. Because `syscall` itself is public (`userspace/src/lib.rs:80`), a production kernel component can call `narf_userspace::syscall::__test_clear_global()`, make the singleton null, and then successfully publish a different table through `try_install_global`. This contradicts the new specification's statements that a published table never changes interceptor identity and that “Production has no removal or replacement operation” (`userspace/specification/spec.md:34-55`). The revised reset does resolve the prior UAF: it no longer reclaims a pointer that an in-flight dispatcher may hold. It does not, however, enforce production one-shot identity. Compile the reset only into an explicit test configuration and make production builds fail to resolve it. Since 616 Rust call sites currently use the helper, that requires gating the test modules/cross-crate verification surface consistently, not merely hiding the symbol from rustdoc. Add a production-configuration compile/API check demonstrating that no reset/removal symbol is available.

- **S2 / major evidence gap — the expanded tests still stop below every live architecture entry and do not prove the newly specified lifecycle ordering.** The host tests invoke local `SyscallTable::{dispatch,dispatch_ctx_versioned}` or private `dispatch_intercepted` (`userspace/src/syscall.rs:5099-5207`). The two kernel smokes do the same (`:5300-5379`); the publication portion reaches only `kernel_syscall_entry_plain`, whose null `user_state` explicitly skips ptrace, timer/signal delivery, and rescheduling (`:5393`, `:3961-3963`, `:4045-4070`). No new test executes `frame/src/x86_64/syscall.rs:369-405`, x86 `int 0x80` at `frame/src/x86_64/trap.rs:1156-1188`, or AArch64 SVC at `frame/src/aarch64/trap.rs:153-178`. There is also no concurrent publication test and the global-identity test distinguishes handler return values, not interceptor identity/callback counts. The local tests now genuinely cover `Complete` suppressing the native handler, a versioned raw/version/RIP/RSP snapshot, typed context-managed observation, sequential publication refusal, and preservation of the first table; those are meaningful additions, but they do not substantiate the spec's ptrace/interceptor/signal/reschedule order or cross-architecture parity. Add a real x86-64 user `syscall` test for known and unknown `Complete` calls with ptrace and a pending signal, common-entry tests for `int 0x80` and AArch64 SVC, a final-result I/O-accounting test, and a concurrent two-publisher test that asserts exactly one interceptor identity remains active.

- **S2 / validation blocker — the exact-head full x86-64 kernel-test gate did not complete successfully.** `cargo xtask test --arch=x86_64` returned 1: two unrelated-looking `userspace/abi-pathx` smokes failed, and the run later page-faulted at `rip=0xffffffffc0000030` immediately after `smoke_module_load_real_ko_round_trip` printed module init and exit, so the harness never emitted its final summary. The touched `userspace/syscall` subset passes, and the failing test source is outside this diff, but this review has no base run proving the failures/crash pre-exist or are independent of changed global-table lifetime/memory layout. Do not treat the narrow pass as a green project gate; establish provenance with a clean base comparison/reproduction and obtain a complete passing required gate before landing.

## Prior-finding resolution and execution-path trace

- **Mutable interceptor authority: resolved.** `SyscallInterceptor` now receives only `&SyscallInvocation`; normal returns and context-managed completions use separate callbacks (`userspace/src/syscall.rs:150-184`). The latter returns `()` and neither callback receives `TrapContext`, so an interceptor cannot mutate RIP/RSP/signal state or convert `ContextManaged` to a return. `dispatch_intercepted` alone applies a returned replacement (`:4897-4932`).
- **Atomic publication and reader UAF: partially resolved.** `try_install_global` uses a null-only compare/exchange and reconstructs only its unpublished losing allocation (`userspace/src/syscall.rs:4738-4758`). The reset leaks rather than frees the retired table, so readers cannot UAF it. The remaining production-visible reset is the blocking identity issue above.
- **Unknown x86 fast-path lifecycle: resolved in source.** `kernel_syscall_entry_plain_with_state` now carries `Option<Syscall>` through the same ptrace entry, intercepted dispatch, ptrace exit, timer/signal, and reschedule sequence (`userspace/src/syscall.rs:3884-4072`). Unknown `Continue` produces `-ENOSYS`/`InvalidOp`; unknown `Complete` receives the same outer lifecycle. The prior early return is gone.
- **Common paths:** x86 `int 0x80` and AArch64 SVC both call `kernel_syscall_entry`, which routes known and unknown numbers through interception; their architecture code performs signal handling after the common dispatcher. Known read/write calls keep I/O accounting outside interception, so the wrapper records the final replacement. These statements are source traces, not executed evidence at this head.
- **Specification:** the updated interface text now accurately describes immutable callbacks, atomic publication, and intended ordering, except for its false claim that production has no reset/removal API.

## Goalpost-moving assessment

- **Assertions or expected values weakened: no.** Against the base, interception tests are additions. Between the two implementation commits, the kernel smoke changes `exits == 3` to `exits == 2`, but this is justified by the stronger split-callback API and is paired with `managed == 1`; `entries == 3` still requires exactly three matching completions. New exact snapshot, native-call-count-zero, publication-refusal, and original-identity assertions strengthen coverage.
- **Tolerance widened, allowlist/exemption added, case skipped/ignored, or comparator relaxed: no.** No changed tolerance, threshold, allowlist, skip/ignore annotation, or comparator admits behavior previously rejected.
- **Failure renamed/relabelled or converted to pass: no.** New and changed tests retain explicit `Fail` branches; no failure classification changed to pass.
- **Check or validation gate deleted: no.** No pre-existing test or gate was removed. The remaining live-path gap is missing evidence, not a deleted base check.

No goalpost lowering was found. The narrow local-dispatch coverage must nevertheless not be relabeled as proof of live architecture behavior.

## Verification

- Resolved and verified exact `HEAD` as `c3a92bbbe38b59649316f5d39ddd0f61fba690ef`; inspected the complete two-commit diff and surrounding dispatcher, x86 fast syscall, x86 `int 0x80`, AArch64 SVC, ptrace, signal, timer, reschedule, I/O-accounting, global-publication, reset, specification, and test paths.
- `git diff --check 87b7cd1048a9492fccb4339558038c06d862e0b4..c3a92bbbe38b59649316f5d39ddd0f61fba690ef`: passed.
- `cargo check -p narf-userspace --tests`: passed.
- `cargo fmt -p narf-userspace --check`: passed.
- `XTASK_QEMU_TIMEOUT_SECS=300 cargo xtask test --arch=x86_64 --subsystem userspace/syscall`: passed, 3 pass / 0 fail / 0 skip, including both interception smokes.
- `XTASK_QEMU_TIMEOUT_SECS=300 cargo xtask test --arch=x86_64`: failed as described above; no final suite summary was produced.
- No AArch64 runtime test was executed. The checkout remained source-clean throughout; this report was written only under `/tmp`.

## Verdict

**CHANGES REQUESTED** for exact head `c3a92bbbe38b59649316f5d39ddd0f61fba690ef`.

This is advisory review only and does not satisfy NARF's required subsystem-owner and maintainer review for an Interface-class change.
