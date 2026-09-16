# Shared-preload SUD component: superseded snapshot, not committed

**Correction (2026-09-04):** I retract the formatting-success claim for this
snapshot. Its retained `final/rustfmt.log` shows a required blank-line removal
at `reverie-preload/src/lifecycle.rs:33`, independently reproduced with exit 1
in `ignored/sud-review-codex-20260904-d8Ms9C/fmt-admission.log`. The original
`final/README.md`, logs, patch and archive remain unmodified historical evidence,
not a valid formatting approval. Independent review requested three changes.
The revision-2 report is `SUD-REVIEW-FIXES.md`; independent re-review found its
P1 fix incomplete for intercepted signal delivery. The latest source-bound
report is `SUD-SIGNAL-DISPATCH-R3.md`, still awaiting independent review. No commit.

## Exact source and ownership

- Slot: `/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec`.
- Branch: `liteinst-exec-sud`; base HEAD `b25d54a97e726eccf276340675d0812b8276028a`.
- Canonical registration: agent `01a06deb-8ee5-7780-b7f6-1747910c5810`, generation 1, actual owner/coordinator PID 1823858, task `liteinst_90_percent_architecture`.
- Canonical create's GitHub fetch was refused. The authorized local named worktree was immediately registered. No stale-worktree tolerance override was needed; unrelated rows and directories were retained. Exact commands/help/refusals/receipt live in the evidence root.
- Old fallback slot remains clean at `7b51e521fcacbbe876c93d1cd0893c442c1a52f6`.
- Review patch SHA256: `2a39ab555d5e0f29c6c4b9839ea344802091dc38a543b3ec21813d9cf6ffec83`.
- Source archive SHA256: `b47b6793cfde9e8c92dcbdffd403e220d2e3b6770a3e41e7139ea2419407f0e9`.
- Evidence: `/home/newton/work/dev-hermit/ignored/liteinst-sud-component-20260904/final`.
- `component.patch` applies cleanly against the unchanged base index (`git apply --cached --check`, no staging). `source.tar` contains the ten changed/new paths plus ignored Cargo.lock and the selected toolchain file. `source.sha256` was checked after validation.

## Product change

Opt-in `lifecycle::InProcessUserDispatch` implements the existing controller
seam. Only the caller's thread is armed, with a null selector and a three-byte
assembler-labeled trusted range (SYSCALL plus RET). Explicit unsafe per-thread
enable/re-arm and disable APIs document handler, alternate-stack, dispatcher,
fork/TLS and code-lifetime obligations. Errors preserve kernel errno; a failed
setup is never a native-success or unavailable-kernel skip.

The public unsafe extern-C `trap::trusted_sigreturn_restorer() -> !` preserves
kernel-expected RSP, sets RAX=15, and tail-jumps to the existing actual SYSCALL
label. It does not call, allocate, look up TLS, synthesize a signal frame, or
create a competing clock wrapper. The linked regression decodes its JMP and
compares the destination to `trusted_gate().syscall_ip`, rather than guessing
an offset. The current default seccomp path and its restorer setup are unchanged.

SUD SIGSYS validates source code 2, x86-64 architecture and saved instruction
address, then normalizes the syscall number into the existing SyscallEvent.
It shares the existing dispatcher and deferred-return path. Seccomp still
requires source code 1. Foreign SIGSYS fails closed. Guest SUD reconfiguration
is refused; a runtime-owned trusted-gate API remains explicitly unsafe.

There are no new dependencies, manifest edits, cache edits, new Tool engine,
LiteInst runtime edits, or changes to exec/static guards. Cargo used a copied
ignored lockfile and an owned private target, offline with the repository's
nightly-2026-07-29 toolchain. No Hermit guest invocation occurred.

## Modified paths (exactly ten)

- `reverie-preload/README.md`
- `reverie-preload/src/bin/user_dispatch_probe.rs`
- `reverie-preload/src/dispatch.rs`
- `reverie-preload/src/lib.rs`
- `reverie-preload/src/lifecycle.rs`
- `reverie-preload/src/signal.rs`
- `reverie-preload/src/trap.rs`
- `reverie-preload/src/user_dispatch.rs`
- `reverie-preload/tests/trusted_restorer.rs`
- `reverie-preload/tests/user_dispatch.rs`

## Final validation at the frozen source

All builds/runs admitted through dagrun: two CPU cores, eight GiB, swap zero,
one active step, private target, 900-second step deadline. The source requires
x86-64 Linux. Probe children install kernel ptrace-denial controls and alarms;
the integration parent also enforces a ten-second child deadline. No full
workspace build/suite or network provisioning occurred.

| Exact command | Result |
| --- | --- |
| `cargo test --offline --locked -p reverie-preload` | 32/32 Rust tests: 25 unit, 4 existing seccomp smoke, 1 restorer, 2 SUD integration; zero ignored/filtered |
| `cargo test --offline --locked -p reverie-preload --features coordinator-rpc` | 46/46 Rust tests: 39 unit, same 7 integration; zero ignored/filtered |
| `cargo clippy --offline --locked -p reverie-preload --all-targets --features coordinator-rpc -- -D warnings` | exit 0 |
| selected-nightly `rustfmt --edition 2024 --check` over all nine changed/new Rust paths | **FAILED**: lifecycle.rs blank line; earlier exit-0 claim retracted |
| `git diff --check`; source SHA256 check; base patch application check | exit 0; no staged paths |

The two SUD integration tests encompass **16 bounded probe cases**: first and
repeated traps; six arguments plus negative errno; failed exec with unchanged
dispatch state; fork re-arm; independent thread re-arm; disable/re-arm; blocked
mask; fault-injected ENOSYS; guest reconfiguration refusal; mask mutation
refusal; re-arm before setup; unchanged exec/execveat refusal; invalid selector;
unreadable selector; foreign SIGSYS; ptrace denial. The unavailable-kernel case
uses real seccomp to inject ENOSYS, not a claim that this host lacks SUD. The
thread case creates the thread while its parent is explicitly disarmed, then
independently arms both; it does not prove general instrumented clone support.

Probe result mutation is through the real shared **SyscallDispatcher**, not a
claimed in-guest Detcore Tool. This crate alone has no typed Guest adapter.
The coordinator-rpc unit tests exercise the adjacent shared driver/RPC helpers,
not an invented combined SUD/Detcore integration result.

Final log modification times: default tests 2026-09-05T00:17:49Z, RPC-feature
tests 00:17:55Z, Clippy 00:18:56Z. Exact logs, admission commands/receipts and
toolchain/host identity are retained. Binary manifests describe retained build
outputs, including the final RPC-feature cdylib, not a falsely preserved
default-feature cdylib from before Cargo replaced that shared output path.

## PMU handoff and limits

The earlier independently consumable two-path restorer patch is frozen at
`../restorer-frozen/restorer.patch` relative to the final evidence directory,
SHA256 `20ccde35542257bba19bcd9934e5ba3bd16279776a1e38dd96c8f519ee40ab80`.
Its README identifies the exact tested binary/full working-tree snapshot and
base-apply check. Its native SUD restorer regression passed 1/1 before the
controller was finalized. This is execution evidence, not review approval.

PMU overlap is `reverie-preload/src/trap.rs`: restorer/gate assembly plus the
subsequent shared SIGSYS normalization helper. `RuntimeEntryHooks` fields and
ownership guard semantics are retained. No `clock_boundary`, `Continuation`,
`BoundaryHooks`, or `clocked_signal!` implementation is duplicated here. PMU
must tail-jump to the public restorer with the kernel's original restorer RSP;
only PMU's reviewed scalar protocol may persist outside its frame borrow.

This component does not enable exec, migrate Tool/config/RPC/log state, cover
native loader/preinit work, or solve clocks. All three historical TSC startup
crashes remain red; their evidence checksums are unchanged. All **340** frozen
reference cells remain, with target **306/340** and no coverage credit here.
No general signals/lifecycle/Detcore/L2/backend-readiness/landing claim.

Independent review is requested for the frozen patch (core syscall interception
model, trigger 2). No commit, push, PR, task closure, or landing yet.
