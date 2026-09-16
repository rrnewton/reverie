# SUD review fixes: revision 2, uncommitted and awaiting independent review

**Superseded:** Independent revision-2 review resolved P2/P3 but found P1
incomplete: intercepted tgkill still failed under SUD. Revision 2 is not
approved. See `SUD-SIGNAL-DISPATCH-R3.md` for the new fix, exact source and retained
failures. The frozen revision-2 patch, archive, report and logs remain unchanged.

## Identity and evidence

- Slot: `/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec`, branch `liteinst-exec-sud`.
- Unchanged base: `b25d54a97e726eccf276340675d0812b8276028a`.
- Evidence root: `/home/newton/work/dev-hermit/ignored/liteinst-sud-component-20260904/revision-2`.
- Complete `component.patch` SHA256: `c3eff21483523a5bc2208053d8f9255718f64711e3b255e840524753c9c05d40`.
- Complete `source.tar` SHA256: `f96779ea783ec2ef52da243a984cf3fc1cb9b8e66bc7445f47290c68838ee7cf`.
- `source.sha256` SHA256: `1eb847a3cb734d91d2de7398f8b1ceac6b0a9b32d7f3b58a048163c38f5a675f`.
- Complete patch includes the previously approved restorer prerequisite; do not apply it twice. No change to its assembly or standalone test.
- Review addressed: `/home/newton/work/dev-hermit/ignored/sud-review-codex-20260904-d8Ms9C/INDEPENDENT_REVIEW.md`.

## P1: Linux signal return, not ordinary syscall forwarding

At `reverie-preload/src/trap.rs:321`, a validated SUD `rt_sigreturn` now sets
the saved instruction pointer to the approved trusted restorer, retaining the
saved stack pointer. Normal SIGSYS return first restores the interrupted
libc-restorer context. The approved assembly then asks the kernel to consume
the still-live application signal frame at the original kernel-expected RSP.
The operation is never forwarded through a Rust call frame. No copied or
fabricated frame, widened trusted range, replacement application handler,
signal prohibition, mask relaxation, or frame pointer retained after return.
The runtime ownership guard leaves normally before SIGSYS return; exact
assembly-boundary clock accounting remains separately unqualified.

`ordinary_libc_signal_returns_under_both_controllers` executes the same libc
SIGUSR1 handler under unchanged seccomp and SUD. Each mode receives four
signals: two on the regular stack with disposition installed before runtime
setup, then two with SA_ONSTACK after libc reinstalls the disposition while
interception is active. Assertions check actual returns, atomic handler count,
errno, restored SIGUSR1/SIGSYS masks, subsequent application syscall result and
balanced runtime hooks. Signal delivery uses the trusted gate, as in the
reviewer's reproduction; this is not a general asynchronous-delivery proof.

Before the fix, admitted `reproduce.json` ran this test: seccomp completed its
assertions, SUD died with SIGSEGV (raw wait status 139), not the reviewer's
SIGBUS variant. The failure is retained in `reproduce.log`, with admission
exit 1 and `reproduce-source.sha256`; it has not been relabelled as success.
After the fix the same paired assertions pass under both feature configurations.

## P2: Linux prctl argument width

`trap.rs:359` classifies the option as `libc::c_int`, matching Linux's int
parameter instead of comparing all 64 register bits. The new unit test covers
matching low bits with zero/nonzero/all-one upper bits, nonmatching options
(including negative int), and a non-prctl syscall. Existing actual guest SUD
reconfiguration refusal stays tested. The added `prctl-forwarded` kernel probe
checks the harmless PR_GET_DUMPABLE query with and without unused upper bits
against the trusted-gate result. No runtime guard-bypass experiment was used.

## P3: explicit retraction and corrected formatting

**I retract the earlier formatting-success claim**, including prior progress
and handoff messages. The original frozen `final/README.md:68` said exit 0,
but `final/rustfmt.log` contains a lifecycle.rs blank-line diff; the independent
`fmt-admission.log` establishes exit 1. Original artifacts remain byte-identical
(`original-artifacts.check`), and `prior-report.md` retains the pre-correction
live report. The live `SUD-COMPONENT.md` now explicitly corrects the claim.

Removed that blank line. A first revision-2 formatting check also found one
new long line in the regression; it failed admission before tests and is
preserved as `attempt1-*`. After fixing that line, the final admitted command
chains formatting, default tests, RPC tests, Clippy and whitespace checks with
`&&`; any nonzero exit prevents admission success. The final rustfmt log is
empty and final admission exits 0. No checker output is ignored or masked.

## Final exact-source checks

All commands run in the registered slot, using the selected nightly, offline
with two Cargo jobs and the exclusively owned external target. `validation.json`
records exact commands; `admission.log` confirms two CPU cores, eight GiB,
zero swap, one step, 900-second step deadline. Native probe children retain
ptrace-denial controls and eight-second alarms, with a ten-second parent bound.

| Command | Observed result |
| --- | --- |
| `rustfmt --edition 2024 --check` on all nine changed/new Rust paths | exit 0 |
| `cargo test --offline --locked -p reverie-preload` | 34/34: 26 unit, 4 seccomp smoke, 1 restorer, 3 SUD integration |
| `cargo test --offline --locked -p reverie-preload --features coordinator-rpc` | 48/48: 40 unit and the same 8 integration tests |
| `cargo clippy --offline --locked -p reverie-preload --all-targets --features coordinator-rpc -- -D warnings` | exit 0 |
| `git diff --check`, source SHA256 check, base patch application check | success; index remains empty |

Neither complete package run ignored or filtered tests. Three SUD integration
tests contain 19 bounded subprocess cases (the original 16 plus two ordinary
signal/controller cases and one harmless prctl query). The default and RPC
runs are alternative feature configurations, not 82 distinct tests. Final log
times: default 2026-09-05T00:39:37Z, RPC 00:39:43Z, Clippy 00:39:44Z.
`binaries/` retains the final RPC-feature probe and cdylib with SHA256 manifest;
the earlier default-feature DSO was not separately preserved.

## Scope and handoff

Exactly five paths changed relative to the first controller snapshot:
`reverie-preload/README.md`, `src/trap.rs`, `src/lifecycle.rs`,
`src/bin/user_dispatch_probe.rs`, and `tests/user_dispatch.rs` under that crate
(the complete base-relative patch remains ten paths).
No other scope added. The complete path list is `paths.txt`; the source archive
also retains the ignored lockfile and toolchain identity. Old fallback remains
clean at `7b51e521fcacbbe876c93d1cd0893c442c1a52f6`.

The P1 handling intentionally follows seccomp's existing unconditional signal
return semantics rather than presenting sigreturn as a Tool callback. This is
shared-dispatcher mechanism evidence, not combined SUD/shared-Detcore evidence.
No general xstate, nested ordinary-signal, arbitrary async PC, exact-clock, exec/loader,
thread lifecycle, L2 or backend readiness claim. Existing seccomp/default,
exec/static guards and all 340 reference rows remain; target 306/340 unchanged.
All three historical TSC startup crashes remain red. No commit, staging,
publication, task closure or landing. Independent full-controller re-review
is required; Jason's isolated restorer approval is not controller approval.
