# Hermit-on-Narf POC implementation progress

Date: 2026-09-24

## Binding implementation target

Deliver two clean components:

1. An upstreamable Narf change that adds principled first-class interception
   hooks for syscalls and, in later phases, other nondeterministic instructions.
2. A general `reverie-narf` backend for arbitrary Reverie tools and supported
   guests. Shared process-global tool state stays directly in Narf/Reverie's
   shared address space; it is not moved through IPC. Interception is a native
   kernel/backend mechanism, not ptrace emulation, signal machinery, binary
   rewriting, or polling.

The completion gate remains one real x86_64 guest with identical observable
behavior and byte-identical versioned fail-closed canonical INFO logs under
Linux/Hermit ptrace and actual Narf/`reverie-narf`, plus a rejected
negative-control mutation.

## Registered work locations

- Parent progress worktree: slot `narf-poc-progress`, generation 1, branch
  `codex/narf-poc-progress`, rooted at
  `/home/newton/work/dev-hermit/worktrees/slots/narf-poc-progress`.
- Hermit worktree: slot `narf-poc-hermit`, generation 1, branch
  `codex/narf-poc-hermit`, initial revision
  `173929ab38290bd3c5c21fcfda91a58284cd4f61`.
- Reverie worktree: slot `narf-poc-reverie`, generation 1, branch
  `codex/narf-poc-reverie`, initial revision
  `1d92000607542a9effc8c7cb4e87ad024e813b79`, now fast-forwarded cleanly to
  `4dc9e0bb80ab9a129430a778d5cfe4213832148c`.
- Narf checkout (as required, outside `dev-hermit`):
  `/home/newton/temp/agent_checkouts/narf-hermit-poc`, branch
  `codex/reverie-interception-hooks`, upstream base
  `87b7cd1048a9492fccb4339558038c06d862e0b4`.

## Narf syscall-interception milestone

Current local Narf head:
`de6f90306fc754410015a9439121ed8bdeb178be`.
The clean upstream hook stack still ends at
`d3cd3255357d9a9675b68e3dcebce3b786b3cdf2`; the top commit is the
POC integration which consumes the separately committed Reverie core.

Commits:

- `70ed22719f190fb8362b0ff94015aa46d35a2518` — first-class,
  table-owned syscall interception and specification.
- `c3a92bbbe38b59649316f5d39ddd0f61fba690ef` — type-safe callback
  authority, atomic one-shot global publication, unknown-syscall lifecycle
  parity, and stronger executable tests.
- `d87869d24fb9b104ed04b3910d9b2289a63d6fce` — make the ordinary
  reset crate-private, expose an out-of-crate reset only under the explicit
  `verification-test-reset` feature, and compile-check that production callers
  cannot name the private reset.
- `b312fdc83ee6d74d0550f4773b94174608c56ae0` — exercise known and
  unknown syscall interception through real x86 `int 0x80`, x86 `syscall`, and
  AArch64 `svc` architecture entries.
- `5d0270978738be5862eeccd25a1822bfe32d4d29` — add typed,
  first-class nondeterministic-instruction interception, initially x86
  `RDTSC` via CR4.TSD, plus per-CPU activation across legacy entry and
  own-stack scheduler migration. This TCB commit is GPG-signed and carries the
  required originating-prompt, model, and Codex co-author attribution.
- `10dc04b5c2757e8fdecd031bebe30688209b5268` — address the first
  exact-head instruction-interception audit: freeze subscriptions at install,
  make the exception callback an unsafe contract with fail-closed recursion
  detection, remove reset APIs from the production boot feature graph, and arm
  every online CPU through a synchronous IPI rendezvous before install returns.
  This TCB commit is also GPG-signed with the required AI attribution.
- `d3cd3255357d9a9675b68e3dcebce3b786b3cdf2` — add the
  kernel-owned native syscall transition needed by a Reverie Guest. The
  original handler is at most once; explicit injected requests carry their own
  wire number and six arguments, may repeat, and bypass recursive interception;
  context-managed outcomes remain terminal and cannot become fabricated
  returns. This commit is GPG-signed with the required AI attribution.
- `de6f90306fc754410015a9439121ed8bdeb178be` — pin the separately
  published `reverie-narf-core`, translate the real Narf native transition,
  keep Tool global and per-task state as direct kernel references, and install
  an opt-in pass-through Tool on the production syscall table. This TCB commit
  is GPG-signed, adds a machine-checkable safety argument, and is deliberately
  above the clean Narf-hook PR boundary.

The current interface provides an immutable `SyscallInvocation` snapshot,
entry control (`Continue` or `Complete`), a return-only replacement callback,
and an observation-only context-managed callback. Interceptors never receive
the mutable `TrapContext`; only the dispatcher can own register, park,
redirection, and signal state. `try_install_global` publishes null-to-pointer
with compare-and-exchange and returns a rejected table without changing the
active identity. The out-of-crate kernel-test reset deliberately retires
without reclaiming old tables, avoiding use-after-free by concurrent readers.
Known and unknown x86 `syscall` numbers now traverse the same ptrace-entry,
interception, ptrace-exit, signal/timer, and reschedule lifecycle.

## Validation at `c3a92bbbe38b59649316f5d39ddd0f61fba690ef`

- `cargo check -p narf-userspace --tests`: passed. Captured output SHA-256
  `8437eb56a8568ff4b1fd5e7013f8dcd6ae329db7e6dc65a942b84408a626ebc2`.
- `cargo check -p narf-userspace --features
  kernel-test,narf-scheduler/kernel-test`: passed. Output SHA-256
  `72790fb20591ffca05fec7bd68ac5aec42c026d5abcb9340c6aa9f7296339775`.
- `cargo clippy -p narf-userspace --features
  kernel-test,narf-scheduler/kernel-test -- -D warnings`: passed. Output
  SHA-256
  `90187a9c57ce9e96d293582bf1f0fb321122ca55b5458cd5e2599e770a9e32da`.
- `cargo fmt --package narf-userspace --check` and `git diff --check`:
  passed. Workspace-wide formatting remains intentionally unclaimed because
  upstream has unrelated pre-existing drift in `memory/src/tests.rs`.
- `cargo xtask test --arch x86_64 --subsystem userspace/syscall --hw-profile
  minimal`: passed in QEMU with clean kernel exit and no panic markers. The
  executed subsystem cases were:
  `smoke_syscall_interceptor_complete_and_global_publication`,
  `smoke_syscall_interceptor_contract`, and
  `smoke_syscall_from_raw_matches_tables`, all `[ OK ]`. Output SHA-256
  `f793ac65d08c3e687d0cd901925ef424db40466b4ba1755ccc94e258b872aa3c`.

The executable tests prove local dispatch, exact versioned snapshots including
RIP/RSP, native result replacement, immutable context-managed completion,
unknown `InvalidOp`, `Complete` suppressing its native handler, and global
publication retaining the original active table after a rejected replacement.
They do not yet prove a real userspace `syscall` instruction combined with
ptrace and a pending signal, AArch64 runtime parity, the future
`reverie-narf` backend, or cross-kernel canonical trace equivalence.

## Live architecture and instruction milestone at `5d027097...`

The current branch now has two independent first-class paths:

- Syscalls enter through their ordinary x86 and AArch64 architecture entry
  paths, capture an immutable invocation, and allow `Continue` or a typed
  completed result without exposing the mutable trap context to the tool.
- Ring-3 x86 `RDTSC` is converted to `#GP` by CR4.TSD. `frame/` guarded-copies
  and accepts only exact opcode `0f 31`, calls the immutable typed instruction
  interceptor, writes EDX:EAX, and advances RIP by two bytes. Every other
  general-protection fault retains the ordinary fault path. No ptrace stop,
  signal trampoline, binary rewrite, polling loop, or IPC transport is used.

CR4.TSD is per-CPU. `arch/` therefore owns a monotonic kernel-wide request;
successful interceptor publication raises it, the legacy user-entry path
applies it before entering ring 3, and the own-stack scheduler applies it
before every switch-in. The scheduler does not own or dereference tool state.
A dedicated QEMU test clears only the executing CPU's TSD bit while retaining
the global request, switches through the real `poll_to_yield` path, and requires
the bit to be restored before the task runs.

Exact-tree validation:

- Package-scoped `cargo fmt --check` for `narf-arch`, `narf-scheduler`,
  `narf-userspace`, `narf-frame`, and `narf-verification`: passed.
- `git diff --check`: passed before the clean signed commit.
- Warning-as-error clippy for the five affected crates with their kernel-test,
  verification-reset, and user-mode E2E features: passed.
- `cargo check -p narf-userspace --tests`, `cargo check -p narf-scheduler
  --features kernel-test`, `cargo check -p narf-verification --features
  user-mode-e2e`, and `cargo check -p narf-frame --features
  kernel-test,user-mode-e2e`: passed.
- `cargo xtask test --arch x86_64 --subsystem scheduler/stackful --hw-profile
  minimal`: returned 0. The new
  `smoke_user_instruction_interception_reapplied_on_switch` case was `[ OK ]`;
  the selection booted and exited cleanly. Raw output SHA-256:
  `ed50698b83d68a90fba1ec7fadd74fd036ea1c5191ef82024df1fd7329351e80`.
- `cargo xtask test --arch x86_64 --features user-mode-e2e --subsystem
  verification/syscall-entry --hw-profile minimal`: returned 0 at the exact
  committed tree. The real ring-3 guest executed an unknown `syscall`, observed
  the hook-supplied result, executed `RDTSC`, observed the return callback's
  exact `0x123456789abcdef0` result, and exited through `int 0x80`; the selected
  test was `[ OK ]` and boot exited cleanly. Raw output SHA-256:
  `22b253d0157a730d58fb9c3aa22267f726efe1650251e42ecd7444688543aeda`.

These tests prove the Narf mechanisms and their real x86 entry paths. They do
not prove that `reverie-narf` exists, that an arbitrary Reverie tool can use the
hooks, that a real executable runs under that backend, or that Linux/Hermit and
Narf traces match. The POC is not complete.

## Audit fixes and SMP evidence at `10dc04b5...`

The separate exact-head review of `87b7cd10...5d027097...` returned **CHANGES
REQUESTED**. Its byte-identical report is tracked at
[`audits/narf-interception-5d027097.md`](audits/narf-interception-5d027097.md)
(SHA-256
`a6ff9dbf2b8241dafba433446da3fc7747204b1acc78f6d1f942dadfa799051f`).
It found three blocking defects: reset capability leaked into production
`boot-init`, instruction subscriptions were mutable and queried multiple times,
and install returned before already-running peer CPUs had armed CR4.TSD. It
also required the exception-context safety contract to become explicit and
called out missing task-identity and full-lifecycle evidence.

Commit `10dc04b5...` addresses the three blockers and the unsafe-callback gap.
The production build now passes while a resolved
`cargo tree -p narf-frame --features boot-init -e features` contains no
`verification-test-reset` match. A distinct `kernel-test` feature owns the
out-of-crate reset capability. The interceptor's family mask is captured once
at installation; the trap path loads one immutable slot and makes one ownership
decision. `InstructionInterceptor` is unsafe with a no-allocation/no-park/
no-reentry contract, and a per-CPU recursion guard stops the kernel rather than
returning an uncontrolled result.

The generic `narf_lib::smp::remote_call` executes a short IRQ-safe function on
the caller and every selected online peer, waiting for every acknowledgement.
Instruction installation publishes its frozen slot, raises the monotonic trap
request, calls the activation function on every online CPU, and only then
returns. Later CPU bring-up and task migration retain the existing switch-in
activation backstop.

Validation at the clean signed commit's source state:

- `cargo check -p narf-userspace --tests`, `cargo check -p narf-frame
  --features boot-init`, `cargo check -p narf-frame --features kernel-test`,
  and `cargo check -p narf-frame --features user-mode-e2e`: passed.
- Warning-as-error `cargo clippy --all-targets` for `narf-lib`,
  `narf-userspace`, `narf-arch`, `narf-frame`, and `narf-verification`: passed;
  no lint was suppressed.
- Package formatting checks and `git diff --check`: passed.
- Two-vCPU x86_64 real ring-3 syscall/RDTSC test: one AP came online, every
  online CPU reported CR4.TSD armed after installation, the selected test was
  `[ OK ]`, and boot exited cleanly. Transcript SHA-256:
  `049dbdf9ac641ea8a2b3039163de3d3b0d6112c60209fcc008372318ecabb37e`.
- Focused two-vCPU x86_64 cross-CPU-call test: one AP came online, both CPUs
  executed the action before return, the selected test was `[ OK ]`, and boot
  exited cleanly. Transcript SHA-256:
  `54c2a989aa899c301bbcef39838c4f2557e6625a5b204ce5aea497dd2d64c909`.
- The same focused cross-CPU-call test passed on two-vCPU AArch64 with one AP
  online and a clean boot. Transcript SHA-256:
  `c6242815c793824c549d9298750ffb49d99cbd8d4d64568a62217baf4343bb0d`.

A broad x86_64 `syscall_abi` selection also passed the new remote-call and
membarrier rendezvous tests, but returned nonzero because two unrelated
existing `/proc` ownership cases failed. Its transcript SHA-256 is
`2f97e07d259c913daf4f018174772e776bf7954babd8ed91f8b2b6be8f1b13f1`.
That broad selection is not claimed green. The focused new test was registered
under `syscall_abi/interception` so its causal result can be run independently;
no existing assertion, skip, comparator, or tolerance was weakened.

## Native syscall transition and Reverie driver checkpoint

At Narf head `d3cd3255...`, `SyscallInterceptor::on_syscall_enter` receives a
borrowed `NativeSyscallTransition` rather than mutable trap state.
`execute_original()` can run the intercepted native handler once and then
returns `NativeSyscallAlreadyExecuted`; `execute_injected(request)` runs the
specified wire number and arguments once per call without recursively entering
the interceptor. Normal injected returns are captured without writing the live
guest return registers. An exit, exec, park, or redirection produces a sticky
`ContextManaged` outcome, after which the dispatcher refuses to fabricate a
normal completion.

Validation:

- `cargo check -p narf-userspace --tests`, frame `user-mode-e2e` check,
  package formatting, diff check, and warning-as-error clippy for userspace,
  frame, and verification: passed.
- Focused x86_64 kernel test proved one original execution, rejection of the
  duplicate original transition, two explicit injected calls with distinct
  arguments/results, final Tool-owned return replacement, and clean boot.
  Transcript SHA-256:
  `8670216e12b69d33481e61d697a4faa6550f472c161d7d8a0e35d0717687e3e8`.
- The existing three-case userspace syscall-interceptor selection passed with
  clean boot after the API change. Transcript SHA-256:
  `2b952d4b31ac555b11bd176475448edda86f0eb3f4ebec38a8778cbf21876ce4`.
- The real two-vCPU x86_64 ring-3 syscall/RDTSC test passed with one AP online
  and clean boot. Transcript SHA-256:
  `44fc3765d570417b2ce564dbc3d989a8c0bd769a393897005593fc35c5932a79`.
- A direct host `cargo test -p narf-userspace` attempt is not a valid runner for
  this kernel crate and failed to link pre-existing Narf symbols such as
  `narf_phys_to_node` and `__text_start`; no host-unit pass is claimed. The
  repository's QEMU kernel-test driver executed the new assertions instead.

The registered Reverie worktree now has commits
`ad1ba781798859a0274678ac9c5ebab23368a0bf`,
`42e7bca681802693f3132d3a2453da8f4851e496`, and
`08a70a2e1562de1602d20e8bf1f6b2238aec7ee5`, adding a new
`reverie-narf` crate. It defines a mechanism-neutral `NarfKernel` trait and a
`NarfGuest<T, K>` that hosts arbitrary `T: reverie::Tool`, including direct
global-state calls, per-thread state, original/injected syscalls, register,
memory, stack, timer, clock, and terminal-outcome surfaces. Its fake-kernel
test passed and proves a Tool mutates thread state, calls the singleton directly
through `send_rpc`, executes the original transition once, and returns the
combined result. `cargo check`, formatting, warning-as-error clippy, unit test,
and doctest gates passed.

`reverie-narf-core` is dependency-free `no_std` code containing the exact
unsigned 32-bit syscall wire number, six arguments, typed native-transition
outcomes, direct global/per-thread state borrows, kernel-compatible Tool
contract, and driver. The ordinary `reverie-narf` crate reuses those exact
request/outcome types instead of maintaining a parallel ABI. Host unit tests,
warning-as-error clippy, and a bare `x86_64-unknown-none` build with Narf's
pinned `nightly-2025-09-14` toolchain passed. The exact head is published only
as feature branch `rrnewton/reverie:codex/narf-poc-reverie`; it is not merged.

The signed Narf integration head consumes that exact Reverie commit and has now
executed in an actual kernel image:

- `cargo xtask build --arch x86_64 --debug --features
  reverie-narf-poc,boot-init` passed and linked the generic adapter into Narf.
- A focused QEMU kernel test passed one original native execution, Tool-owned
  replacement, direct shared global state, and persistent per-task state:
  `1 pass, 0 fail, 0 skip`. Transcript SHA-256:
  `d34db716b0f337aeff4e67f49e1d3f02a1351bd09c6140ce2c6cc7a64efd7f39`.
- A bounded production boot printed
  `INFO reverie-narf-poc backend=v1 state=active` from the first real
  userspace syscall, then continued through `NARF Userspace Init started!` to
  the login prompt before the external 150-second timeout stopped QEMU.
  Transcript SHA-256:
  `baebb8ddcf49544fb254edaf7a2d71893c165eedc87b798dd61aa054408785a0`.
- Package formatting, warning-as-error clippy, host check, and bare-metal
  x86_64 check for the adapter passed. A standalone host `cargo test` is not a
  supported Narf runner and failed on the same kernel-linker symbols documented
  above; the assertion ran and passed through Narf's QEMU kernel-test driver.

This is the first working Narf-linked syscall path through the new backend
core, but it is not the completed POC. The opt-in Tool is only pass-through;
the existing full `reverie::Tool` API still depends on Linux/`std`, ordinary
asynchronous suspension currently fails closed rather than parking on a Narf
waker, and task teardown does not yet reclaim adapter thread state. There is no
versioned canonical cross-kernel trace, Linux/Hermit comparison, or negative
mutation result yet. The production-boot activation line is diagnostic
evidence, not the required canonical INFO log.

## Audit state

The first separate adversarial review covered exact head
`70ed22719f190fb8362b0ff94015aa46d35a2518` and returned **CHANGES
REQUESTED**. Its byte-identical report is tracked at
[`audits/narf-syscall-interception-70ed227.md`](audits/narf-syscall-interception-70ed227.md)
(SHA-256
`145c1bf6fb0d0f46cb0b0631341b1e08ee561a16c41813003df242cc118e7d07`).
It identified four substantive gaps: mutable trap-context authority, replaceable
global identity and unsafe reset reclamation, an early-returning unknown syscall
path, and insufficient executable coverage. Head `c3a92bbb...` addresses the
first three and adds the bounded tests listed above. A fresh review at exact
head `c3a92bbb...` again returned **CHANGES REQUESTED**. Its byte-identical
report is tracked at
[`audits/narf-syscall-interception-c3a92bb.md`](audits/narf-syscall-interception-c3a92bb.md)
(SHA-256
`e5823d11c21a5697ab4bfc37842acd13cb6c088fbf46db51b2444f051f3995b1`).
It confirmed the mutable-context, reader-UAF, and unknown-syscall fixes, then
found the still-public reset as the remaining blocking interface issue. Commit
`d87869d...` fixes that issue. The review also retained major evidence gaps for
live architecture entries, concurrent publication, and final I/O accounting;
those are not relabeled as proven.

The third report at exact head `5d027097...` is the instruction/SMP audit linked
above. Head `10dc04b5...` fixes its blocking production-feature,
subscription-TOCTOU, cross-CPU activation, and safe-trait findings. A fresh
exact-head review is still required after the syscall transition boundary is
implemented; the earlier report is not treated as approval of the changed head.

## Full-suite failure provenance

The full x86_64 kernel-test gate is not green. The same module-unload page fault
reproduces on the untouched Narf base and on implementation head
`d87869d24fb9b104ed04b3910d9b2289a63d6fce`:

- Base `87b7cd1048a9492fccb4339558038c06d862e0b4`:
  `XTASK_QEMU_TIMEOUT_SECS=300 cargo xtask test --arch=x86_64` returned 1 after
  `smoke_module_load_real_ko_round_trip` printed module init and exit, then
  page-faulted at `rip=0xffffffffc0000030`. Output SHA-256
  `c1301d0bd423fcc6deb9d8bbce0e8ea141e019a12b05603bc319b7b12ea3e15a`.
- Head `d87869d24fb9b104ed04b3910d9b2289a63d6fce`: the same command returned
  1 at the same test and instruction address after the same init/exit sequence.
  Output SHA-256
  `e1c068c984f404dcc8af61ef9b314f280f8e2553e2b2a71a75be41972fee0d1f`.

This establishes that the module-unload crash predates this branch; it does not
turn the full gate green. The focused syscall QEMU gate at `d87869d...` passed
all three selected cases and exited without panic. The temporary detached base
worktree was removed after its output was captured under `/tmp`.

## Next gates

1. Split a `no_std` kernel-facing core from `reverie-narf`, retain the arbitrary
   Reverie Tool adapter on the `std` side, and compile the core for Narf.
2. Implement the Narf adapter against the signed native transition and direct
   task/memory/register interfaces; then add subscription and lifecycle state.
3. Complete the `Backend` launch/lifecycle contract and the minimal Hermit
   integration/tool surface in their registered worktrees.
4. Run the real guest on both backends, compare fail-closed versioned INFO logs
   byte-for-byte, and show the negative mutation is rejected.
5. Obtain exact-head adversarial and deterministic-scheduling reviews where
   applicable, open the upstream Narf PR for human owner/maintainer review, and
   land only through each repository's supported policy.
