# Private native-effect stepping

The default-off `test-owned-cpuid` feature retains every old finite prediction
and adds a separate unsafe `__install_owned_compiled_step_fixture` admission.
This is an integer execution increment, not arbitrary dynamic-guest admission.
Public SUD refusals, Detcore subscriptions, clock source and timer controller
are unchanged. No PMU sampling/skid correction or asynchronous signal support is
introduced: the existing shared counter observes every owned TF completion.

Iced 1.21 decodes in paused ordinary runtime. The explicit mnemonic/operand,
encoding, implicit-register and memory-access gates admit ordinary GPR integer
effects, scalar loads/stores, near control transfers and ordinary stack use.
The CPU supplies destination values. Unchanged GPR bits and flags, zero-extension,
successor, exact branch delta, stack transition, call return-address store and
instruction bytes are independently checked. A memory-indirect near call/jump
requires a bounded 64-bit pointer read through the same checked DS/SS address
resolver as other scalar memory accesses. Its destination must be executable.

Capture authenticates the existing owner/frame/request/sequence and #DB/error
contract. It seals the complete 440-byte native prefix and 2444-byte FP area
(2440 xstate plus trailer) before relocation or runtime execution. Final Q and
HookContext must match that independent seal; only the actual relocated FP
pointer and owned TF change are permitted. Guest TF is refused. RF at native
entry needs the authenticated preceding instruction-fault resume PC; a completed
ordinary instruction must have cleared it. TF is never installed in runtime R.

The reusable seal is separate from history, cannot be recycled before final
verification, and does not allocate in capture. New admission explicitly chooses
zero diagnostic history or a checked, preallocated byte budget. Overflow is
terminal and keeps the committed prefix. Old finite history and terminal APIs
retain their old 133120-byte limit and behavior. The new pure regression verifies
64 distinct completions and full images, retaining 266240 bytes; another exhausts
an explicitly insufficient budget without dropping evidence.

Stable RX mappings, no writable code alias/mutator, single-thread ownership,
trusted fresh-exec startup, no POSIX timers since exec, no future asynchronous
sources and the existing signal-frame lifetime preconditions remain unsafe
caller duties. Available timer inventory must be empty; unavailable inventory
is reported as unavailable, never inferred empty. Bounded instruction fetch joins
only readable executable intervals and never reads an inaccessible suffix.

PUSHF, POPF, IRET, MOV/POP SS, debug shadows, REP/string progress, LOOP/JRCXZ,
far transfers, FS/GS memory, FP/vector instructions, TSX, privileged/unusual
system instructions, JIT publication, general faults, blocking/restarted syscalls,
lifecycle and asynchronous guest signals remain unsupported. Existing subscribed
CPUID/TSC and finite SIGSYS cancellation reuse their typed Tool/RPC paths and
are not credited as retired TF steps. Native zero/default XSAVE signal controls
keep every payload byte and non-permitted header field exact, while explicitly
allowing XSTATE_BV bit 7 to clear only when both complete Hi16_ZMM payloads are
zero, as permitted by AMD's ordinary XSAVE definition. This native-control
comparison does not relax the owned-step frame seal or admit another runtime
FP profile; a seeded profile remains additional, bounded evidence only.

Callback timer windows check bounded opcode/form eligibility without consulting
pre-result operands. Final arming decodes and validates operands, memory effects,
successor mappings and flags after typed callback results are installed, before
publishing TF. A timer request is not permission to execute an invalid final
continuation. Finite-profile decoding and timer/controller policy are unchanged.

The standalone fixture uses compiler-generated scalar arithmetic, volatile array
memory, runtime-selected function calls and a real counted loop, not assembly
substitutes for its workload. Assembly only brackets the work and real event
instructions. Its whole-clock oracle comes from the frozen optimized linked CFG:
one initial Jcc and 32 loop Jccs, no Jcc in either mixing function. Expected timer
clocks are 2 through 32 by twos with full three-instruction suffixes; final clock
is 33. This is not the old fixture's startup-seven boundary. Linked-code review
must verify these facts anew before execution. Runtime work variations, repeated
runs, native output/pipe once-only checks and raw images are retained separately.
No native qualification is claimed merely because the fixture compiles.
CPUID output depends on its fixed leaf/subleaf inputs; TSC output depends on the
fixed request kind. All-event timer/RPC ordinals remain observations only. Armed
and unarmed runs require the same exact instruction-result vector, with a direct
cross-run comparison and no timer-count subtraction or normalization.

The integration target builds its guest offline and locked with the pinned
`nightly-2026-07-29` release profile and all features of the launcher, runtime and
RPC transport. It retains a copy, executable hash, compiler identity, command
and Cargo artifact metadata; a failed build has no existing-binary fallback.
The ordinary Cargo profile still builds and tests the original target. Loader
variables are removed only from child commands and recorded; the guest still
rejects `LD_*`, `GLIBC_TUNABLES` and any `/etc/ld.so.preload`. A short external
`REVERIE_COMPILED_STEP_ARTIFACTS` directory remains optional; without it the test
creates and retains a private temporary directory. Existing output is not
overwritten. The exact optimized CFG and 33/553 checks remain required.
No host arrival order, Detcore CLI, L2 or backend-completeness claim follows.

The manifest adds an explicit iced decoder/instr_info dependency and a gated
binary. Cargo manifests are generated upstream; no authoritative local Buck/
autocargo input for this crate is available. Regeneration must retain that exact
dependency feature set, existing export markers and default-off fixture gating.
