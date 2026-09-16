# SUD signal-dispatch correction: revision 3, awaiting re-review

## Frozen identity

- Slot `/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec`, branch `liteinst-exec-sud`.
- Unchanged base `b25d54a97e726eccf276340675d0812b8276028a`; no commit or staged paths.
- Evidence `/home/newton/work/dev-hermit/ignored/liteinst-sud-component-20260904/revision-3`.
- Complete `component.patch` SHA256 `fcdf2184d726559458be0e7a499c642d88e4b9ece1c4bd2aa54132ece1594e77`.
- Complete `source.tar` SHA256 `48112f9c9c19ed5940b40aaf0059c45ab3fb86157dfedc5e20b33786bb147bc3`.
- `source.sha256` SHA256 `7dc613a71076b4a0d013c83e3e20c7df6b24d5a57fe702b081e41205bac3ac3e`.
- Complete patch includes the unchanged approved restorer assembly/test. Do not apply that prerequisite twice.
- Review addressed: `/home/newton/work/dev-hermit/ignored/sud-r2-review-codex-20260904-9kACOn/INDEPENDENT_REVIEW.md`.

## Root correction and ownership

`defer_to` only replaces saved RIP; it is not a generic machine-state-preserving
dispatcher continuation. No arbitrary post-signal thunk or fabricated signal
frame was introduced. Instead, the existing synchronous dispatcher now has an
explicit SUD forwarding scope:

1. SUD's SIGSYS action masks ordinary signals during runtime dispatcher work.
   `user_dispatch.rs:39` records a stack-local activation borrowing only the
   original live frame's saved signal mask; TLS points to this activation and
   retains the previous activation pointer.
2. `SyscallEvent::forward` reaches the same trusted gate through
   `user_dispatch.rs:52`. It marks the current activation suspended, then
   installs **exactly the saved guest mask**, not an empty mask, before the
   real syscall. This applies to every forwarded syscall, not a tgkill special
   case. Runtime-internal `raw_syscall6` calls do not open guest-delivery scopes.
3. An ordinary handler delivered there may perform intercepted syscalls.
   `trap.rs:304` permits nesting only while the parent is suspended; each nested
   activation has its own kernel frame, scope, and runtime entry/leave guard.
   Returning from it restores the prior handler flag and activation pointer.
4. After the forwarded syscall returns, the runtime masks ordinary signals
   again, captures the actual resulting guest mask into the original frame,
   and resumes dispatcher exclusion. Kernel signal return then restores that
   mask. Existing valid rt_sigreturn handling still uses the approved restorer
   and original saved RSP. No frame/mask pointer survives its activation's
   signal return. No whole-register or xstate copy is introduced.

Dispatcher implementations must not retain a lock or exclusive shared-state
borrow across `forward`: an ordinary guest handler can invoke the same shared
dispatcher while its outer activation is suspended. This obligation is now
documented. Ordinary signals can be pending during runtime-only work, but are
not held blocked across forwarded blocking syscalls. Guest-installed action
masks and the guest's original blocked set are retained.

The initial mask/ownership fix exposed an undersized inherited alternate stack.
Retained diagnostics show size 8192 and only 368 bytes between the diagnostic
helper's RSP inside the handler and the stack base. `signal.rs:103` now ensures the same
minimum stack size already allocated by the existing seccomp helper (at least
65536); larger stacks are reused. This reserves adequate space for the tested
nesting, not arbitrary depth. Seccomp's handler policy/allocation size is unchanged.

## Regression evidence and preserved failures

`reproduce.json` ran the new tests against revision-2 runtime source, before
the fix: trusted-gate delivery passed; intercepted delivery and nested guest
syscalls died with SIGSYS (raw wait status 159). Source hashes and a complete
pre-fix source archive accompany the failed log/admission.

All old assertions remain. New tests in `tests/user_dispatch.rs` cover:
- Same libc-installed handler under seccomp and SUD, both trusted-gate and
  intercepted libc tgkill delivery. Four returns per mode, regular/alternate
  stacks and dispositions installed before/after interception remain checked.
- A dispatcher counter proves intercepted tgkill actually reaches shared
  dispatch; trusted-gate delivery must not increment that counter.
- SUD intercepted delivery whose ordinary handler makes nested application
  syscalls. A non-native getuid result proves nested shared dispatch. Exact
  masks check guest-blocked SIGUSR2, handler-blocked SIGTERM and automatic
  SIGUSR1 blocking, with SIGSYS available; errno and balanced hooks remain checked.
- A forwarded blocking read interrupted with exactly `-EINTR`. A native periodic
  SIGUSR1 is only a functional fixture; the read end stays empty and the write
  end open. This detects accidentally retaining runtime masking across the read.
- A unit test checks stacked dispatcher exclusion/restoration.

Iteration evidence is not rewritten as success: attempts 1/5 failed formatting;
attempts 2/3/4/6 retained SIGSEGV failures while diagnosing alternate-stack
exhaustion; attempt 7 passed tests but failed Clippy on an unused import. Removed
that import before final validation. Logs include fixture-only mask/stack/entry
diagnostics. `original-artifacts.check` and `revision-2-artifacts.check` confirm
the earlier frozen evidence is unchanged, including the previous formatting
failure and its explicit retraction. Re-review P2/P3 resolutions are retained.

## Final exact-source validation

`validation.json` chains all commands with `&&` and records admission exit 0.
Offline selected-nightly builds use the exclusively owned external target,
two Cargo jobs, two CPU cores, eight GiB, zero swap and a 900-second step bound.
Probe children retain ptrace denial/eight-second alarms and ten-second parent
bounds. No full workspace/matrix, Hermit CLI execution, or network work.

| Command | Result |
| --- | --- |
| selected-nightly `rustfmt --edition 2024 --check` over all nine Rust paths | exit 0, empty log |
| `cargo test --offline --locked -p reverie-preload` | 38/38: 27 unit + 4 seccomp smoke + 1 restorer + 6 SUD integration |
| same command with `--features coordinator-rpc` | 52/52: 41 unit + same 11 integration |
| `cargo clippy --offline --locked -p reverie-preload --all-targets --features coordinator-rpc -- -D warnings` | exit 0 |
| whitespace, source hashes, complete patch application against base index | success; index empty |

No ignored/filtered tests in either final package run. The six SUD integration
tests encompass 23 bounded subprocess cases. Feature configurations overlap;
38+52 is not a count of distinct tests. Final log times: default
2026-09-05T01:05:14Z, RPC 01:05:21Z, Clippy 01:05:22Z. `binaries/` retains the
final RPC-feature cdylib and probe with hashes; no claim to a separately retained
default-feature DSO. Commands, failures, final logs and manifest are external.

## Scope, overlap and limits

Seven paths changed relative to revision 2: `reverie-preload/README.md`,
`src/bin/user_dispatch_probe.rs`, `src/dispatch.rs`, `src/signal.rs`,
`src/trap.rs`, `src/user_dispatch.rs`, and `tests/user_dispatch.rs` in that crate.
The complete base-relative patch still has ten paths, listed in `paths.txt`.
Old fallback remains clean at `7b51e521fcacbbe876c93d1cd0893c442c1a52f6`.

This is shared-dispatcher SUD signal-delivery evidence, not Detcore, L2, general
signal handling, arbitrary nesting, nonlocal handler exits, all restart cases,
or asynchronous machine-state qualification. Seccomp nested guest syscalls
during intercepted delivery are not newly qualified. RuntimeEntryHooks ABI
and the trusted-restorer assembly are unchanged; PMU integration must review
these mask/ownership transitions and exact assembly boundaries separately.
No clock reset/relaxation, broader trusted range, SUD disable, native backend
substitution, Tool migration or exec enablement. Exec/static refusals, all 340
reference rows and the 306/340 target remain. Three prior TSC startup crashes
remain red. Full controller approval is still pending; no commit or landing.
