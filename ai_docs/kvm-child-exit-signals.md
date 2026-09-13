# Tool-selected normal child-exit signals

This component adds `Guest::queue_child_exit_signal` so a Tool can hand a selected normal child-exit event to its current KVM receiver. It does not generate automatic child notifications, change wait/reaping policy, call a host signal API, or qualify Hermit scheduler integration. The private base is `a8d87e6a15f527e1c683e99faef2979f7c9ecfcb`; production and focused API controls were frozen at `6f14b930410652f694f9bc01b68ab521f60aa2bf`. Subsequent changes add live controls and this scope report. Both sources remain preserved locally; no public pull-request head changed during this component.

## Receiver contract

The caller authenticates the selected parent, child incarnation, normal exit status, UID and CPU accounting. The event's process target is the backend-visible current `Guest::pid()`, while `siginfo.si_pid` is the child identity visible to the guest. Backend validation checks metadata shape and receiver identity; it cannot substitute for that caller authentication.

KVM accepts only process-directed `SIGCHLD` with `CLD_EXITED`, zero `si_errno`, positive child PID, status 0 through 255 and nonnegative CPU clock fields. The current receiver must be its registered process leader, at its current task generation, with no live sibling or prepared thread creation. Thread targets, other producer codes, stale/wrong process identities and unsupported callback contexts refuse before effects. Existing syscall, signal and captured page-zero-fault boundaries supply the return transport. Initial thread-start, initial exec and post-exec callbacks refuse. No injection guard or existing private deferral refusal changes.

Publication takes lifecycle, process signal and thread signal locks in that order. It uses the existing process pending set, retaining the first complete 128-byte siginfo when a standard signal coalesces. It records the existing disposition generation, not a scheduler operation or delivery ID. Explicit `SIG_IGN` suppresses child-exit generation even when blocked. Default-ignore remains observable to the Tool; `SA_NOCLDWAIT` does not suppress that observation. A blocked event remains pending. Eligible acceptance is not a promise of guest handler execution or `EINTR`: the existing Tool callback, disposition and return path still decide delivery.

After releasing all state locks, the operation updates matching virtual signalfd readiness. Missing descriptor state is a typed failure before publication; an actual readiness error after insertion or coalescing returns `FailedAfterCommit` with the original errno and generation. The caller must not treat that as a guest syscall error or blindly retry it. Queueing performs no guest instruction, recursive Tool callback, scheduler RPC or child join. Signalfd creation requires nonblocking mode, and existing descriptor flag handling preserves that restriction.

Fork receives empty pending state; exec retains accepted process events and the mask while replacing registration atomically. An outstanding process event prevents creating a competing unsupported thread consumer. The new controls exercise these existing transitions and conditional generation checks rather than introducing another queue. This does not expand multi-thread-parent or arbitrary running-vCPU delivery.

The exact event returned by `handle_structured_signal_event` undergoes the same child metadata validation and retains process ownership if blocked again. Signalfd encoding now includes child status and both CPU fields, in addition to PID and UID. The old timer and other supported producer encodings remain unchanged. Other backends default to typed unsupported refusal; `IntoGuest` forwards all outcomes and event bytes unchanged.

## Linux behavior and live evidence

Linux v6.17 `kernel/signal.c`, retained at `/tmp/astra-reverie-sibling-signal-linux-v6.17.c` (SHA256 `074166696c837c48ba5edc1fcefa4dd3e4b0736b2093545b0824a3f60f9c18e5`), distinguishes explicit ignore suppression from normal process-directed child notification in `do_notify_parent`. Its waitability decisions are separate from pending-signal delivery. The actual C fixture checks native wait status and reaping independently of the new Tool operation: ordinary children exit 37, the second coalescing child exits 43, each waitable child is reaped once, and explicit ignore/`SA_NOCLDWAIT` preserve `ECHILD` and the untouched status sentinel.

The fixture first waits for an actual child, then emits one marked `getpid`. Native Linux has already generated its signal; the test Tool verifies the actual consuming child exit, then exercises the explicit queue operation at that boundary. This proves receiver behavior, not an automatic backend producer or Hermit's control protocol. The selected 11/13 CPU ticks test transport and encoding, not the caller's CPU-accounting calculation. Native PID, UID and CPU values are checked according to their actual environment; they are not falsely compared with synthetic Tool identities.

| Mode | Required behavior |
| --- | --- |
| 0 | Default-ignore still reaches one Tool signal hook; no guest handler. |
| 1 | One caught event; exact child fields and all 128 supplied siginfo bytes in the guest frame. |
| 2 | Blocked event consumed once by signalfd, exact 128-byte record, complete surrounding buffer unchanged, second read `EAGAIN`. |
| 3–4 | Explicit ignore suppresses generation whether unblocked or blocked; no hook or pending event. |
| 5 | `SA_NOCLDWAIT` auto-reaps independently while retaining caught signal delivery. |
| 6 | `SA_NOCLDSTOP` does not suppress a normal exit notification. |
| 7 | Blocked pending bit and zero handlers before unmask; exactly one handler afterward. |
| 8 | Tool suppression observes the exact event and suppresses the guest handler. Native still delivers normally. |
| 9 | A valid Tool replacement changes status to 43 in the complete supplied frame. Native retains actual status 37. |
| 10 | Queue from an actual SIGUSR1 Tool callback; both original signal and child handlers complete once. |
| 11 | Queue from the captured page-zero-fault Tool callback; actual fault handler restores the original path and child delivery completes once. |
| 12 | Tool-returned status 256 fails with original `EINVAL`; no successful parent exit is recorded. Native unchanged event succeeds. |
| 13 | Two actual child exits coalesce; signalfd keeps the first child's complete record, both wait statuses remain correct, and no second signal remains. |

Every successful Tool case requires unique consuming child and root thread/process hooks with exact statuses. Mode 12 checks the existing malformed-event error policy; it does not claim to add consuming cleanup to unrelated runtime failures. All cases retain the guest's five-second alarm and the existing exact-test subprocess's 30-second plus two-second kill bound. Native children also use a seven-second plus two-second kill bound. No Hermit binary ran for these backend controls.

Signalfd-before-fork remains a concrete excluded native/KVM difference. The initial live fixture created the descriptor before fork: native succeeded and KVM returned `ENOSYS` before the new API ran. The failure is retained at `/tmp/astra-reverie-child-exit-controls/live-receiver-single-marker.{json,stdout,stderr}`. The final receiver fixture creates signalfd after wait, while SIGCHLD remains blocked, preserving every ABI, sentinel, consumption and wait assertion. It does not claim descriptor inheritance support. The existing refusal tests remain unchanged.

The first fixture also emitted a second apparent marker because a following zero-argument libc `getpid` retained unused argument registers. The exact one-publication assertion failed with 2 versus 1. The fixture now obtains the comparison PID first with explicit zero arguments, then emits its one marker. The original failure remains in `live-receiver-hook-signature-fixed.stderr`; the required publication count was not increased. Three earlier Rust test compilation errors and the initial forwarding-mutation compilation error remain recorded separately and are not runtime evidence.

## Verification and negative controls

With `CARGO_TARGET_DIR=target/astra-maturity-reviewed` and `REVERIE_REQUIRE_KVM=1`, the following completed:

- `cargo test -p reverie-core -p reverie-kvm --all-features --all-targets`: 665 passed, zero failed/ignored, 15.138 seconds including rebuild. Groups contain 14, 4, 1, 384, 3, 2, 248, 3 and 6 tests.
- `cargo clippy -p reverie-core -p reverie-kvm --all-features --all-targets -- -D warnings`: passed in 4.523 seconds.
- `cargo fmt --all -- --check`: passed in 1.316 seconds.
- `cargo check --workspace --all-targets`: passed in 7.029 seconds. Existing vendored native build warnings remain in output; this is not a claim that all native components rebuilt in that elapsed time.

The source was restored byte-for-byte after each negative control. Seven deliberate production defects each failed the unchanged intended test with an assertion and exit 101: old signalfd encoding (3.971 seconds), private rather than process pending ownership (3.620), replacement of first siginfo while preserving the claimed coalescing result (3.770), suppression of default-ignore Tool observation (2.473), relabelling a post-publication error as pre-publication refusal (3.922), accepting invalid Tool-returned status (3.321), and omitted `IntoGuest` forwarding (2.419). The first forwarding mutation did not compile because its diagnostic patch omitted a type qualification; that attempt remains unqualified. The corrected mutation produced the expected actual assertion failure.

Commands, statuses, source hashes and logs are in `/tmp/astra-reverie-child-exit-controls/`. The live fixture and every native/Tool result are under `target/astra-child-exit-signals/full-required-kvm/`. The exact seven mutation patches, copied binaries and outputs are indexed by `/tmp/astra-reverie-child-exit-mutation-summary.json`; complete drivers are `/tmp/astra-reverie-child-exit-mutations.py` and `/tmp/astra-reverie-child-exit-forwarding-mutation-fixed.py`. Final source and copied-binary identities are `/tmp/astra-reverie-child-exit-final-identity.json`; the test source preservation proof is `/tmp/astra-reverie-child-exit-test-preservation.json`.

Final copied KVM library binary SHA256 is `cf234b84570294694de9fcea5d3abd493b59fa6ea648be8325f81ddb4562c174`; static ELF test binary is `0fd15cd2fe9f4e76b0256f3258e2266fbe66c92608e17f33671e1bb898e5ff56`. Both are under `target/astra-child-exit-signals/final/`. GCC was `11.5.0 20240719 (Red Hat 11.5.0-15)`, invoked by the existing helper as `/usr/bin/gcc -O2 -pthread child-exit-signals.c -o child-exit-signals`.

All preexisting executor and runtime test bytes remain identical after removing only the new module/include additions. The full 476,570-byte original `static_elf.rs` and 6,802-byte core signal-bridge test are unchanged prefixes. No old assertion, comparator, case, bound, unsupported-producer refusal or injection guard was weakened. The complete suite includes the existing terminal cancellation, exec-error, worker lifecycle, signal and SCM_RIGHTS isolation controls.

## Review and remaining scope

The applicable post-facto-review rule at `hermit/.claude/skills/post-facto-review/SKILL.md` is trigger 2: “A Reverie `Tool`, `Guest`, `Backend`, syscall-interception, or other core API abstraction change.” The new typed Guest operation is that API change. Independent Codex and Claude review of the final source remains required before public landing. This report is implementation evidence, not author self-approval.

No scheduler policy or Hermit capability predicate changed. The earlier full/partial timeout evidence and the later combined SIGCHLD INFO mismatch remain failed historical results until an actual newly qualified composition supersedes them. This backend operation does not by itself qualify strict scheduling traces, record/replay, general asynchronous signal support, signal-death or stop/continue producers, multi-thread parents, or interruption of arbitrary host-blocked waits. KVM retains its own return-frame implementation; no gVisor code or syscall interception path was imported.
