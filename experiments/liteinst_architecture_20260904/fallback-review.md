# Unpatchable syscall fallback: independent review request

Review the uncommitted `liteinst-syscall-fallback` worktree against Reverie
`320412c5967790939ebe405c73e394ffd9c41459`, with LiteInst2
`95ee5e6917fa33191eb41c3f1606ea8b03c1b78c`. This is a source-snapshot review,
not a clean-HEAD validation receipt. No commit or landing is authorized yet.
The frozen patch, changed sources, lockfile, hashes, commands and logs are in
`/home/newton/work/dev-hermit/ignored/liteinst-syscall-fallback-20260904/review-freeze/`.
`fallback-freeze.json` in this experiment directory records the archive hashes.

## Observed execution and scope

Final rebuilt `rpc_tool-7ce51afa73642c97` rerun, 2026-09-04T21:52:46Z:
- `unpatchable_syscall_dispatches_tool_after_signal_return`: 1/1, zero ignored.
- `installed_hook_reentry_bypasses_tool_with_shared_coordinator_rpc`: 1/1,
  zero ignored. Existing instruction publication branches reported two positive
  outcomes, zero refusals; no assertion, hardware branch or skip was changed.
- Shared `reverie-preload` syscall-driver unit selection `tool_host::tests::`:
  10/10, zero ignored, 27 deliberately outside this narrow selection;
  2026-09-04T21:53:24Z. Built offline with `--no-default-features
  --features coordinator-rpc --lib` to test the embedded runtime helpers.
- Focused Clippy with `-D warnings`, final no-run rebuild, touched-Rust rustfmt
  check and `git diff --check` succeeded. No whole-workspace test claim.

The runtime regression creates an RX page ending in `syscall; ret`, verifies
those original three bytes remain unchanged, and requires six traps and zero
installed hooks. It calls the real in-process `install_tool::<FallbackTool>`
and shared Tool dispatch/RPC driver, not a helper-only simulation. Assertions
require getpid result 424242 (distinct from native), seven global RPC callbacks
from one sender, repeated calls, all six arguments, EPERM, tail injection and
ERESTARTSYS retry, registers/red-zone and XMM0, and libc errno preservation.
Nested Tool-internal syscalls intentionally use the existing trusted native
gate; the outer guest calls demonstrably do not bypass the Tool. The test
captures stdout and asserts the exact `fallback: calls=6 rpc=7 hooks=0
bytes=unchanged abi=preserved` line and empty stderr; successful harness logs
are retained, not a separate uncaptured child stdout log.

The test executable's guest embeds the typed runtime as a Rust dependency.
A built cdylib hash is provenance only, not evidence that this test loaded that
DSO. This is a test Tool, not Detcore or Hermit CLI qualification: no L2,
in-process 90%, arbitrary-program readiness or static-support claim. The
frozen reference population remains 340 and the target remains at least 306.

## Machine state and continuation ownership

References below are paths in the frozen Reverie snapshot:
- `reverie-preload/src/trap.rs:182`: the actual kernel SIGSYS frame is borrowed
  only within the existing handler. Its RIP is redirected; `rt_sigreturn`
  restores the kernel-saved guest state before the ordinary fallback starts.
  No kernel-frame pointer is retained or reconstructed. That borrow is invalid
  after `rt_sigreturn`; the subsequent assembly frame is a different object.
- `reverie-liteinst/src/syscall_fallback.rs:17`: one per-thread pending record
  copies only original and continuation PCs and owns a private return stub.
  Initialization precedes interception (`src/tool_host.rs:145`). RX stub and
  RW state pages are separate, process-lifetime, inherited privately on the
  supported single-threaded fork path. There is no general nested-continuation
  stack or busy/generation guard. Guest callable signal handlers/extra threads
  are restricted by the existing runtime; asynchronous reentry into this
  pending record is NOT established safe. Nested Tool syscall dispatch returns
  before `prepare` (`src/runtime.rs:2721`), so the tested recursion does not
  overwrite the pending continuation. Timer integration must address windows
  before/after the Tool callback guard, not merely rely on that guard.
- `reverie-liteinst/src/syscall_fallback.rs:148`: assembly saves guest registers
  below the 128-byte red-zone and restores actual RSP from its saved frame.
  `HookContext.stack_pointer` and `instruction_pointer` remain metadata, not
  setters controlling machine restoration. Return uses the private stub's
  continuation jump without consuming a guest return address. Syscall ABI
  outputs are RAX result, RCX post-syscall PC, R11 saved flags; this is not the
  preserve-all contract of arbitrary-PC asynchronous interruption.
- `reverie-liteinst/src/syscall_fallback.rs:35`: XSAVE uses enabled XCR0 bits
  masked by `0x2e7`: x87, SSE/MXCSR/XMM, YMM upper halves, AVX-512 opmask/ZMM,
  PKRU; otherwise FXSAVE64. AMX, MPX and other excluded components are not
  covered. No general AVX/AVX-512/AMX guest test is claimed: this regression
  deliberately clobbers/checks XMM0 only. Callback/libc AVX use is covered only
  for state selected by this mask; callbacks must preserve excluded state.
  No separate clean callback FP environment is installed. Adequate writable
  guest stack, compatible CPU/XCR0 and ordinary no-unwind ABI are obligations.
- `reverie-liteinst/src/syscall_fallback.rs:123`: libc errno is saved/restored
  around shared dispatch. FS/GS bases and arbitrary TLS memory are not saved;
  callback code must preserve TLS bases and normal runtime TLS invariants.
  The errno test is not proof of arbitrary TLS switching support.

## Small proposed timer interface (not implemented or approved)

1. At the shared signal ingress, **before** changing kernel-frame RIP, pass a
   synchronous borrow of the original live kernel frame plus event kind,
   signal/code and original PC to timer-owned classification. Syscall resume PC
   differs from the retry PC for an instruction fault. Do not pass HookContext
   as a substitute or dispatch an ordinary async Tool callback inside SIGSYS.
2. The decision is either normal existing signal handling or timer-owned
   handling while that signal activation remains live. A frame reference must
   not survive its `rt_sigreturn`. Any copied continuation data needs explicit
   ownership; copying just a pointer is invalid. Both paths eventually use the
   same Tool/RPC syscall driver, not a timer-local syscall emulator.
3. Expose exact code intervals for `fallback_entry..fallback_entry_end` and
   the instantiated per-thread return stub (not just its source template),
   together with per-thread guest/runtime phase and continuation ownership.
   Proposed serialization/state transitions still need review; current single
   pending state is not an async reentrancy guarantee.
4. Timer must own provenance of internal TF and distinguish actual guest
   SIGTRAP/TRAP_TRACE. PUSHFQ makes TF guest-visible; this patch does not solve
   that. Equal retired-branch counts do not prove the same execution position.
   All assembly entry/return boundaries must be accounted for explicitly:
   current Rust callback clock brackets miss them. No calibrated offsets,
   altered clocks or assumed precision are accepted.

Ordinary syscall fallback can be reviewed independently now; exact accounting,
TF handling, async preservation and shared fault handoff remain unqualified.
Review should specifically challenge single-record reentrancy and stack/xstate
obligations. No source was copied to/from the logging or timer slots.

## Required independent review

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely
skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

The existing adjacent test is unchanged; the new runtime test adds assertions.
Request independent correctness review of the frozen patch and execution
evidence, including the limitations above. No reviewer is delegated by this
worker; coordinator to assign review. Keep the task open and do not commit.

Earlier `test-attempt1-link-limit.log` records an infrastructure mistake: a
file-size guard applied around linking caused SIGXFSZ. It is retained, not
relabeled as a passing attempt. Subsequent builds omit that linking guard;
direct test execution keeps the 64-MiB file guard, core dumps disabled, admitted
two-CPU/eight-GiB cgroups and bounded execution. No network or full suite ran.
