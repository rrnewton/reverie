# Unwired process-pending alarm primitive: source handoff

Implementation author handoff, not an independent review or runtime approval.

Repository: Reverie. Write checkout: `/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918`. Branch: `codex/kvm-setitimer-20260918`. Base and current HEAD: `99d1e4827cce2404442d7c27ab447886a5839326`. Source changes are staged, not committed or pushed.

Frozen patch: `SOURCE.patch`, SHA256 `4e56817a6e754e31e69626c184ff2703dc3632d6c23618efb4e1cbc72734a90d`. `SOURCE_INPUTS.json` binds every changed file. The patch contains eleven paths listed below.

## Behavior

`Guest::queue_process_alarm_signal` has an explicit unsupported default and forwards through IntoGuest, KvmGuest and the static ELF executor adapter. It accepts only process SIGALRM with the canonical complete SI_KERNEL siginfo: all bytes other than signo/code zero. The existing private defer API and its provenance semantics are unchanged.

Before publication the executor checks target identity, exit and pending process actions, current task AND process generations, sole live thread-group leader, and matching signalfd descriptor presence. Publication holds lifecycle, process-signal, then thread-signal locks. The adapter admits only transported syscall/signal/fault/thread-entry boundaries and refuses lifecycle/initial-exec callbacks. It also conservatively refuses a callback after any completed injected process action, including a returning fork, because that callback has consumed its original transport. This is Unsupported/ENOSYS, not a claim that the live task exited.

Accepted state always belongs to shared_pending. Standard-signal coalescing retains the first complete existing siginfo, including software ALRM metadata. The receipt separately records blocked status, ignored/caught/default-fatal disposition, pending generation and coalescing. A readiness failure after publication returns that receipt plus the original errno; a pre-publication refusal carries a distinct error class and errno. Ignored alarms remain available for Tool observation, and blocked+ignored generation remains pending. Later SIG_IGN installation uses the existing generation invalidation rules.

The actual Tool-filter return path now passes the dequeued PendingSignalDomain to preparation/reblocking. It does not infer shared ownership from Process provenance. This also preserves the domain of other already-shared signals traversing that common path; historical private defers stay private.

No scheduler call site uses this API. No host kill fallback, dequeue notice, timer arm/rearm, wait completion, signal-frame code, guest clock, turn accounting or comparator changed.

## Authored controls and evidence limits

Eight executor tests exercise complete process/thread pending queues (including full siginfo and per-entry generations), dispositions, signal masks, pending generations, signalfd masks/readiness and logical clock snapshots. They cover the six mask/disposition combinations, all 120 required-zero siginfo bytes, producer/target refusals, first software siginfo coalescing and private-defer independence, blocked+ignored versus later SIG_IGN, live sibling and pending creation/action refusal, retired/reused task and process-generation refusal, pre/post-publication readiness failures, actual pending-domain reblocking, and fork/exec pending lifetime.

Two runtime adapter tests exercise KvmGuest and IntoGuest forwarding across receipt states/failures without guest syscall/injection or child-start effects, and explicit refusal from a default KVM executor.

One initialized-VM test contains nine guest modes. It manually publishes two alarms at a real getpid callback boundary, then checks caught delivery, blocked/unblock, ignored Tool observation, blocked+ignored followed by caught action, later SIG_IGN discard, actual Tool reblock with shared-pending sibling-creation refusal, Tool suppression, default fatal action with exact ExitStatus::Signaled in consuming exit hooks, and signalfd consumption without a handler. It also checks refusal in thread-start, initial exec and post-exec callbacks. It uses the existing static_elf 30-second self-exec bound. This fixture is an API/frame/Tool-path control, NOT a setitimer test, periodic-rearm test, parked-wait continuation test, or a Hermit determinism result.

Executed: rustfmt on changed Rust sources (raw status 0); `git diff --cached --check` (raw status 0). NO compilation, test execution, C compilation, VM run, foreign cache copy, or independent review has occurred. The eleven test declarations are not eleven passes.

The product-local AGENTS.md is absent from this checkout. Parent AGENTS.md (including its tail canary), Codex coordinator guide, and frozen native report/research inputs were read. No instruction/skill file changed.

## Proposed qualification

Root coordinates the separate isolated qualification checkout and its maintained resource bounds. This author requests no target/cache copy and has created neither `target/` nor `Cargo.lock`. A private target directory and one build at a time with two Cargo jobs are sufficient in principle; build time/disk have not been measured. Required host facilities are usable `/dev/kvm`, `/usr/bin/gcc`, the pinned installed `nightly-2026-07-29` toolchain, and the crate's normal dependencies. Each VM is 256 MiB and modes execute sequentially. The maintained 30-second VM subprocess bound is unchanged. Do not disable it or treat a timeout as a pass.

Use the root-selected isolated qualification directory for both working directory and target. If no authenticated lockfile is supplied, Cargo must resolve a new ignored Cargo.lock; retain its bytes/hash as qualification input. Start offline; report missing dependencies rather than silently borrowing a foreign target. If network acquisition is needed, root uses the whole-command with-proxy route.

Exact focused Cargo selectors (root sets CARGO_TARGET_DIR to the private qualification target and CARGO_BUILD_JOBS=2):

```bash
cargo +nightly-2026-07-29 test --offline -p reverie-kvm --lib --test static_elf --no-run
cargo +nightly-2026-07-29 test --offline -p reverie-kvm --lib process_alarm_signal_ -- --test-threads=1 --nocapture
REVERIE_REQUIRE_KVM=1 cargo +nightly-2026-07-29 test --offline -p reverie-kvm --test static_elf process_alarm_signals::process_alarm_signal_tool_boundary_contract -- --exact --test-threads=1 --nocapture
```

The native/KVM requirement is deliberate: absence of KVM must fail qualification, not use the fixture's ordinary optional-host skip behavior.

Required nearby regression selectors for the shared Tool-domain signature change:

```bash
cargo +nightly-2026-07-29 test --offline -p reverie-kvm --lib child_exit_signal_ -- --test-threads=1 --nocapture
cargo +nightly-2026-07-29 test --offline -p reverie-kvm --lib blocked_tool_replacement_requeues_without_consuming_another_event -- --test-threads=1 --nocapture
REVERIE_REQUIRE_KVM=1 cargo +nightly-2026-07-29 test --offline -p reverie-kvm --test static_elf child_exit_signals::child_exit_signal_native_and_tool_receiver_contract -- --exact --test-threads=1 --nocapture
```

Root should capture each raw status before inspecting output, retain full logs and the compile/test binary identities, then arrange both independent reviews at the frozen source. These commands are proposals, not executed results. No broader test success or landing is claimed.

## Remaining obligations

The original KVM setitimer failure remains unresolved. All four native review findings remain open at the Hermit integration boundary: actual shared-SIGALRM dequeue must drive periodic rearm; ignored/suppressed Tool observation must retain the original parked wait; transport needs its exact RPC-origin capability; getitimer interval state is wrong. This patch supplies pending publication only. It does not qualify multithread recipient selection or unsupported callback contexts, periodic consumers, runtime error propagation to a scheduler receipt, timer determinism, or disabled manifest cells.

Changed paths:

- reverie/src/guest.rs
- reverie/src/signal.rs
- reverie-kvm/src/executor.rs
- reverie-kvm/src/runtime.rs
- reverie-kvm/src/signal.rs
- reverie-kvm/src/child_exit_signal_tests.rs
- reverie-kvm/src/process_alarm_signal_tests.rs
- reverie-kvm/src/process_alarm_runtime_tests.rs
- reverie-kvm/tests/static_elf.rs
- reverie-kvm/tests/support/process_alarm_signals.rs
- reverie-kvm/tests/fixtures/process_alarm_signal.c
