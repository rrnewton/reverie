# Native owned-context return: proposed acceptance and ownership contract

Read-only design checkpoint, 2026-09-04 PDT. No product changes, new execution,
kernel experiments, network or delegation. The independently approved positive
fixture is preserved. This document proposes requirements, not activation approval.

## Decision first

**Authenticity plus immutable bytes is necessary, not sufficient.** A conditional
proof of faithful acceptance is possible if every environmental and architectural
condition below holds through the final user return. Neither the committed parser
nor the current LiteInst signal handler enforces that complete invariant. An
ordinary native CPUID fault cannot currently acknowledge a pending owned return:
failed restoration can deliver SIGSEGV/SI_KERNEL with the requested PC/SP already
installed. Treating the instruction bytes at that PC as evidence of execution
would dispatch Tool on an ambiguous failed return.

Propose an explicit per-thread return state machine, a restricted typed result
editor, and early signal classification. Keep reuse disabled until both acceptance
and completion are proved. A concrete register-preserving execution witness is
described below, but its RF/TF and exact-clock limits prevent declaring it a
general solution or authorizing arbitrary instruction callbacks today.

## Pinned evidence

All product references below are immutable Reverie
`8bb9f79350955d791f6f08f315ad0e30bb74afc3`, inspected with `git show` in
`/home/newton/work/dev-hermit/worktrees/slots/liteinst-timer-delivery`, not its
mutable working files. Hermit integration remains shared Detcore, not a new engine.

- K = `/home/newton/work/dev-hermit/scratch/kernel-return-abi-review-lbWRd7/REPORT.md`.
  K:38 records restore order; K:51 segments/flags; K:53 altstack;
  K:64 FP; K:74 forced faults/IRET; K:88 acceptance versus completion.
- Kernel image SHA256 `e4ca09342f074c21080a6d31d17fb7ad6f5fbe2c7b84e14759acfd97e16e0db2`,
  build ID `fa15f90afce5930f146e2138d2ba10eb95e3e5ad`, release
  `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`. Compiled native return at
  `ffffffff821d6140`, size0x250; this is not a general Linux-version theorem.
- P = `reverie-preload/src/signal/native_frame.rs:1`: explicitly not provenance
  authentication or return permission; `:49` explicitly not a consumption token;
  `:93` relocation; `:160` format checks; `:193` XSAVE metadata checks.
- `reverie-liteinst/src/runtime.rs:2602`: current instruction handler decodes
  through `instruction_at`, subscriptions and patch sites; siginfo is used in
  refusal diagnostics, not a sufficient successful-path source discriminator.
  `:2650` callback-active native execution must NOT receive an ambiguous return
  failure or a guest-handler callback under this proposal.
- `reverie-liteinst/src/instruction_event.rs:19`: exact opcode lengths and
  checked addition, not executable-mapping/provenance validation. `:45` result rules.
- `reverie-liteinst/src/tool_host.rs:222` retains no-patch instruction/vDSO
  subscription refusal; `:520` shared typed instruction dispatch and existing
  Tool/ThreadState/RPC; `:567` CPUID and `:584` TSC callbacks.
- `reverie-liteinst/src/syscall_fallback.rs:95`: existing ordinary syscall
  dispatch clears pending state at `:121`, BEFORE machine return. That lifetime
  is not a reusable owned-kernel-context protocol and must not be copied as one.
- `reverie-preload/src/clock_boundary.rs:47/:88/:114/:133`: original-frame
  control scope, scalar hooks, wrapper and live-frame lifetime. Existing kinds
  remain Guest0/Runtime1/HookIngress2; no implicit new success meaning.
- `reverie-liteinst/src/clock_control.s:114/:153/:182`: token handoff and actual
  enable suffix. Its TLS address helper at `:10` uses a GPR and ADD; it cannot
  simply be called after all guest registers/flags have been restored.

## Acceptance conditions and enforcement sites

Every row is required; a passed parser or hash discharges only part of this table.
“Enforce” below describes proposed checks/ownership, not code already present.

| Condition from the return/error analysis | Proposed enforcement and remaining boundary |
|---|---|
| Exact native ABI/return environment | Installation validates native64, qualified kernel/layout and CPU feature profile, user shadow stacks disabled, no compat/x32 return. Never use the on-disk address-limit placeholder as a limit. Unknown profile refuses. |
| Authentic live source | Signal-entry assembly supplies untouched original entry SP; classifier checks signal source, phase/owner, native frame/ucontext relationship and live A bounds before exposing bytes. Only that activation can construct capture permission. A caller-supplied address or siginfo value alone is not authentication. |
| Complete readable fixed prefix | Check440-byte prefix, entrySP relation F=SP-8 and all additions/spans before copying. Source lifetime ends at original handler return; no pointer into it survives. Destination has separate ownership. |
| Correct relocation | Retain original guest pointers, stack descriptor, FP instruction/data addresses, segments and flags. Only the FP storage pointer moves. Q/FP are disjoint from A/R and have qualified alignment and readable extents. Native return uses Q's exact restorer position, not sizeof(libc ucontext). |
| No silent FP reset/downgrade | Reject null FP and unsupported/legacy profiles unless separately qualified; check exact independent standard-format size/features, both magic values, header/reserved constraints, component extent and trailer. Retain entire captured FP image without normalization. Kernel null/reset and malformed-metadata legacy fallback are NOT success alternatives. |
| Architecturally valid FP payload | Obtain it only from the genuine kernel capture; preserve it verbatim. Metadata validation does not validate every XRSTOR component. No Tool writes to MXCSR, x87, PKRU or other captured components through the result API. Unknown layout/component refuses. |
| Compatible current FPU policy | Freeze feature permissions, XCR0-compatible profile, user_size/user_xfeatures/XFD policy through both returns. Getauxval/XCR0/GET_XCOMP snapshots are evidence, not an atomic kernel-state lease. Limit initially to the qualified fixed profile; no dynamic permission requests, AMX growth or unqualified XFD transitions. Any unsupported state change refuses. |
| Accessible immutable Q/FP throughout kernel reads | Preallocated initialized anonymous owned storage, no unmap/remap/protection/pkey changes, no aliases allowed to write after seal, no reuse on Drop or Tool completion. Allocation ownership must also exclude admitted nested frames. Page residency checks or hashes do not guarantee future access. Unrecoverable read/XRSTOR faults are terminal. |
| Valid segments and faithful flags | Preserve valid native CS/SS and strict-SS policy; no LDT/compat transitions. Kernel OR3 or SS substitution is not exactness. Only mask0x50dd5 of saved RFLAGS replaces live flags: prove the remaining bits match required native state at the gate. Do not silently clear guest TF/RF/DF to enter runtime; runtime preparation affects only the first, original context. Unqualified flag/debug modes refuse. |
| Valid final return target | Typed nextPC uses exact decoded instruction length, checked addition, qualified user address and native executable mapping; unchanged RSP remains real guest state. Exclude runtime/witness/guard destinations. Pin mapping/code identity for the operation; a /proc snapshot is not a lifetime guarantee. IRET/target-access errors are not a normal return acknowledgment. |
| Correct altstack despite ignored errors | Keep the installed owned A descriptor already correct, flags0, no AUTODISARM, valid size/permission policy. Q-entry SP is outside A. Do not rely on rt_sigreturn applying a changed uc_stack: setting errors can be ignored. Check/install policy in ordinary context and prevent subsequent descriptor changes. |
| A/R capacity and no overlapping nested frame | Proven linked handler/runtime stack bounds, signal frame bound for admitted xstates, explicit maximum nesting and actual SP-based selection. Check live A ranges, including near-bottom reselection, not merely guard presence. Q lifetime must extend through every admitted nested kernel read. Existing one-shot8192-byte result is not a general nested bound. |
| Faithful masks and signal policy | Retain actual guest mask in Q, ancestor masks per activation, and runtime-only mask during ordinary work. Installation must refuse masks/dispositions incompatible with owned synchronous traps; never silently unblock everything. Interpose subsequent changes or refuse them. No ordinary guest handlers/callbacks in this bounded mode. Forced synchronous failure may defeat blocking, so masks are not failure containment. |
| Native controls and errno | Cache safe TLS/errno access before activation; save errno before runtime work, restore after cleanup. Balance CPUID/TSC scopes and preserve FS/GS, PKRU and access to A/Q/runtime state. No faultable Rust or native CPUID/TSC after final controls are restored. Immutable Q does not restore FS/GS. |
| Instruction results only | CPUID writes zero-extended EAX/EBX/ECX/EDX; RDTSC writes EAX/EDX and preserves RCX; RDTSCP additionally writes ECX with the existing aux=None convention. All other GPRs/flags/segments/FP/SP/mask remain captured. Advance by checked2/3 bytes; do not carry the return target in an output register. |
| Restart/AX semantics | Native rt_sigreturn disables restart-block restart, sets orig_ax=-1 and publishes restored AX. Restrict this new contract to supported instruction faults; do not silently apply it to restartable syscall/tail-injection continuations. Preserve existing syscall tests and define that extension separately. |
| Clock and ordinary runtime ownership | Original frame borrowed only until handler returns. Carry the existing scalar token to actual R entry once; Tool/RPC/allocator ownership exists before taking their locks. Finish destructors and errno/control restoration before final assembly. Count every enabled branch boundary exactly; neither clock leave nor equal RCB acknowledges return. |

Software can enforce the typed editor, phase transitions, checked ranges and its
own call graph. Several lifetime conditions are **unsafe caller requirements**
until protected by a real mechanism: no guest stores into runtime memory, no
unmediated mapping/TLS/control changes, no extra in-process threads and no guest
callbacks while runtime owns state. SUD does not intercept WRPKRU/FSGSBASE or
ordinary stores. Rust borrows, snapshots and syscall interception alone do not
make those assumptions enforced for arbitrary native programs. State that limit
on any private installation API; do not expose a safe general CLI capability.

## Proposed ownership and signal classification

One live generation per admitted thread; generation counters never wrap/reuse
silently. States: Idle -> Captured -> ReturningToRuntime -> Runtime -> Sealed ->
ReturningToGuest -> ExecutionConfirmed -> Idle. Terminal failure has no outgoing
edge. A generation records owner TID, original event, source-frame activation,
owned storage, masks/control expectations and the existing clock token. No Send/
Sync transfer or arbitrary mutable byte view; ending a Rust borrow cannot free Q.

Actual R entry, not its target PC in a signal frame, retires the original A
activation. Dispatch exactly once through the existing shared Tool on R. Seal
typed outputs and retain Q plus the generation through the final gate. Completion
must establish no kernel/handler still reads Q before recycling it at a later
owned entry. No “clear pending” after the callback, timeout-based reuse, overwrite
on recursive capture, or leaking each generation as a substitute for reusable
support. If capacity/provenance is unknown, terminate rather than dispatch again.

Classification must precede opcode decoding, Tool-internal forwarding, allocator
entry and ordinary logging. Use original live signal data plus phase/generation,
exact owned entry/finalizer/restorer address ranges and trusted source checks.
The shared syscall IP is insufficient because several operations use it.

| Signal/position while a return is pending | Required result |
|---|---|
| SIGSEGV/SI_KERNEL, even at intended CPUID with expected SP | Ambiguous: terminal, keep generation unreusable. No Tool callback and no native instruction forwarding. |
| FP failure, SIGBUS, malformed context, invalid native return or unexpected synchronous fault | Terminal by a bounded TLS/allocation-independent trusted exit path; never resume a guest-looking frame. The kernel may already have changed mask/altstack/GPRs and cleared FP. |
| Signal in R/finalizer/shared gate/Q-switch interval | Owned runtime interruption only if exact source, entry PC, generation and nesting permit; otherwise terminal. Never a guest execution acknowledgment. No blanket return-as-Guest classification. |
| Asynchronous notification already pending before return | Not progress. Preserve ownership; only a separately admitted handler may resume the same runtime continuation. Current ordinary-handler restriction remains. Unknown/stale notification terminates this bounded mode. |
| SIGSYS with correct SUD discriminator | Necessary source validation, not a generic consumption proof. Reject internal rt_sigreturn as a guest event; do not wait for a convenient later syscall to avoid handling consecutive CPUIDs. |
| Next CPUID fault after independently confirmed execution and no intervening unqualified return | Only now apply the CPUID provenance/controls/opcode checks, retire old ownership safely, capture next generation and dispatch the same Tool. |

Why this matters: native bad-frame handling forces SIGSEGV/SI_KERNEL without a
unique attempt token (K:76). FP failure installs target GPRs first; common return
publishes AX=0 (K:83). Zero is also a legitimate CPUID input, and initialized FP
may resemble cleared FP. Trap metadata/PC/opcode matching has no demonstrated
disjointness from that failure path. Current instruction_sigsegv_body does not
even enforce the proposed full source predicate. Repeated CPUID must not be
turned into unsupported-as-pass or dispatched based only on a plausible target.

## Can a real execution witness close the gap?

**Conditional design, not an implemented or generally admissible solution.** For
an admitted single in-process thread with stable native mappings, a fixed runtime
assembly entry could perform one RIP-relative immediate store to its active
attempt's completion field, then an indirect RIP-relative jump through its sealed
nextPC field. Both locations are preallocated; no lookup, call, push, scratch GPR,
vector instruction, guest-text patch or runtime stack use is needed. The kernel
return would target that fixed entry with otherwise restored guest state, then
the jump would transfer to the checked nextPC. This changes the internal return
route explicitly, not the guest's instruction result or guest text.

The store is bound to the sole active generation by exclusive ownership: publish
owner/generation/target and reset completion before arming, prohibit stale entry
or concurrent attempts, and never permit guest control flow to target the entry.
Only executed assembly writes completion; a signal handler cannot manufacture it
from PC/SP. Before that store, any ambiguous return fault terminates. Signals at
the store/jump are classified as runtime and cannot dispatch Tool or recycle Q.
A later genuine CPUID can then observe completion without pretending its own
SI_KERNEL provenance is uniquely different from bad-frame SIGSEGV. Every other
intervening runtime signal return would need equivalent ownership/failure rules.

This is NOT ready to implement as a general witness:

1. Ordinary MOV/JMP preserve arithmetic flags, GPRs, FP and stack, **but RF and
   TF have instruction-execution semantics**. An inserted instruction can consume
   RF suppression or generate an extra single-step trap. Debug/watchpoint behavior
   also matters. An RF=TF=0/no-debug subset might avoid that issue, but the current
   evidence does not show all real CPUID captures satisfy it. Rejecting RF/TF
   captures is a scope restriction, not solving their return. Never clear those
   captured bits or mask TRAP_TRACE and call the result faithful. This architectural
   concern is not a newly measured kernel outcome.
2. One-thread RIP-relative storage is concrete. General per-thread DSO TLS is not
   a free substitution: the current GOTTPOFF helper needs scratch GPR/flags. A
   register-preserving per-thread address scheme needs a separate linked proof;
   do not change guest FS/GS, reserve a guest register or silently share one slot.
3. A zero-conditional-branch store/jump suggests no extra **RCB** for those two
   instructions, not exact placement or complete clock correctness. The entire
   enabled finalizer/gate/return/witness interval, first guest branch, pending
   notifications and internal-TF windows need the clock owner's instruction-level
   proof. Pausing through the first guest branch, calibrating an offset, or adding
   synthetic time is forbidden. Current kinds0/1/2 contain no generation-completion
   protocol; do not pretend a new enum value or counter equality supplies one.
4. The witness proves executed user code after return, NOT faithful FP/altstack
   restoration. Silent legacy downgrade/ignored altstack changes can execute it.
   The full acceptance invariant remains required. Store/jump faults, stale owner,
   access loss or interrupted-return ambiguity are terminal, not consumption.

Direct return to nextPC could avoid the inserted-instruction issue only after a
reviewed, enforced acceptance theorem rules out the ambiguous failures, or another
nonperturbing disjoint execution witness is established. No such theorem or
witness is established by the existing parser/positive fixture. No ptrace,
hardware-breakpoint or extra interception mechanism is silently proposed here.

## Exact proposed write set and review order

1. `reverie-preload/src/signal/native_frame.rs` plus its tests: add a restricted
   consuming editor/finalizer over validated owned bytes. Proposed operations
   accept typed `InstructionResult` and a private checked resume target; expose
   no mutable FP/segments/flags/SP/mask. Preserve current pure relocation API and
   tests. Return permission is a separate private runtime token, not a property
   of `RelocatedFrame`. No arbitrary raw-address public execution API.
2. New `reverie-liteinst/src/owned_context.rs` plus private module registration:
   `NativeReturnAdmission`, thread-bound `OwnedContext`, explicit `ReturnPhase`,
   checked generation transitions and `classify_return_signal`. Proposed methods
   capture, apply_result, seal and classify/retire; retirement requires a private
   execution proof plus no live readers. No Drop-to-free while return is pending.
   Pure transition/editor tests can be written before unsafe execution is enabled.
3. `reverie-liteinst/src/instruction_event.rs`: extend checked targets with mapping/
   native-address admission and typed CPUID/TSC result rules, preserving existing
   RCX/aux semantics. Decode supplied validated bytes; do not scan or patch sites.
4. `reverie-liteinst/src/runtime.rs`, shared continuation/fallback module and its
   assembly: **unavoidable owner overlap** for early phase-aware classification,
   capture-before-first-return, ordinary entry, fail-closed exits and final
   machine-state return. Do not change the old syscall path into the new protocol
   accidentally. The fixed witness, if selected after review, belongs here with
   exact symbol boundaries; do not copy a second Tool engine into shared preload.
5. `reverie-preload/src/clock_boundary.rs` and `reverie-liteinst/src/clock_control.rs`
   /`.s`: coordinated scalar ownership/entry-label changes only after the clock
   owner proves both returns and any witness interval. Existing signal masks,
   trajectories, first intervals and optional patched-mode cases remain intact.
6. `reverie-liteinst/src/tool_host.rs`: reuse `dispatch_instruction` and its
   existing Tool/ThreadState/CoordinatorRpc. Bridge a captured register snapshot
   to the existing callback, extract only typed permitted outputs (or refactor
   the private dispatch return type without changing the public Tool contract).
   No duplicate Detcore. Keep current instruction/vDSO capability refusal until
   each real subscription is satisfied; CPUID work alone does not admit Detcore.

Next review should choose whether the witness subset can cover actual instruction
captures without changing flags, and specify the enforceable native-state/memory
contract. Then require consecutive CPUID->CPUID, CPUID->RDTSC/RDTSCP real Tool/RPC
generations with non-native outputs and unmodified text, followed by shared Detcore
only when its remaining subscriptions are implemented. Failure-provenance,
silent-degradation, nesting/Q-window, storage-reuse and exact-clock tests remain
mandatory; none were executed or waived by this read-only task. The approved
normal positive fixture is a prerequisite, not that acceptance test suite.
