# No-patch instruction callbacks: bounded write-set checkpoint

Read-only, 2026-09-04. Basis: `SUD-ONLY-DETCORE-PLAN.md`. Inspected immutable
Reverie `8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675`, tree
`f507be189dfcf7d85d6cb962987dee61c98851e1`, in
`/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec`; worktree clean.
Detcore references remain Hermit `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`.
No Pasteur slot read or modified; no product edits, builds, probes or delegation.

## Decision and smallest write set

**No independent end-to-end instruction continuation can be implemented solely
in disjoint files.** The current syscall return consumes RCX and fixes length at
two bytes. CPUID and RDTSCP produce RCX; RDTSC must preserve it. A separate copied
return engine would duplicate precisely the machine-state/clock ownership that
Pasteur is changing. Keep one shared continuation implementation.

| Proposed path | Smallest responsibility | Independence |
|---|---|---|
| New `reverie-liteinst/src/instruction_event.rs` | Pure classification of safely supplied bytes into existing CPUID/RDTSC/RDTSCP kind, length and checked resume PC; describe input/output registers; inline unit cases | Can be authored separately after authorization; no signal handler, allocator, kernel controls, assembly or second Tool engine |
| `reverie-liteinst/src/lib.rs` | One private module declaration | Small registration edit, subject to owner confirmation |
| New `reverie-liteinst/src/bin/rpc_tool_guest/instruction_fault.rs` | No-patch branch of instruction fixtures using existing Tool/CounterGlobal and reusable assembly cases | Source/test preparation can be disjoint; real execution cannot precede shared integration |
| `reverie-liteinst/src/bin/rpc_tool_guest.rs`, `tests/rpc_tool.rs` | Minimal fixture registration/test entry; retain original patched cases | Narrow test glue, coordinate rather than overwrite |
| `reverie-liteinst/src/runtime.rs` | Validate instruction SIGSEGV without patch arenas; choose no-patch deferral before claim/install; scoped runtime-native instruction controls; dispatch existing instruction Tool callback | **Unavoidable Pasteur overlap**, owner-integrated only |
| `reverie-liteinst/src/syscall_fallback.rs` or Pasteur's replacement shared continuation module | Tagged syscall/instruction pending event, independent resume PC and real all-non-output-register return; exclusive ownership and clock transfer | **Unavoidable Pasteur overlap**; do not copy its assembly into a private instruction module |
| `reverie-liteinst/src/tool_host.rs` | Capability admission once actually implemented; preserve existing instruction result/RPC dispatch | Integration edit; keep vDSO/timer/clock refusals until their own requirements are met |

Thus disjoint work can start only as pure helpers and test source after a new
implementation authorization, not as an independently shippable live handler.
No Cargo dependency, shared Detcore policy or new generic Tool API is required
for the instruction-description helper. Its caller must establish memory safety
and fault provenance; parsing a byte slice does not prove either.

## Exact execution seam and clock dependency

Existing kernel-control setup is `runtime.rs:712/:822/:850`. Existing
`instruction_sigsegv_body` (:2418) decodes through `arena_for` (:2349), then
claims/patches a site. Replace only that route for explicitly selected no-patch
instruction support; preserve the patched route as a separate configuration.

Required route: validated fault -> exclusive shared pending instruction event
-> shared scalar signal continuation -> existing trusted rt_sigreturn -> ordinary
saved-context entry -> `tool_host::dispatch_instruction` (:300/:496) -> the same
`Tool::handle_cpuid_event` or `handle_rdtsc_event` -> restored guest state and
independent checked resume PC. No site plan/install, native guest answer or
duplicate Detcore emulation. Runtime-only nested instructions/syscalls may use
the existing scoped internal behavior only under the declared lifetime ownership
contract; a guest handler must never be classified as Tool-internal.

**Clock agreement needed before executable integration:**

- An instruction event carries original PC, exact length/kind and independent
  resume PC. It cannot encode return in an output GPR or treat HookContext SP/IP
  metadata as actual restoration. Preserve flags, actual stack/redzone and the
  supported FP/xstate set; do not extend the old xstate claim by implication.
- One thread owns one pending continuation; define when it is consumed/released
  and how recursive faults refuse without overwriting. No original kernel frame
  reference survives its rt_sigreturn.
- Shared `clock_boundary::Continuation` kinds remain Guest=0, Runtime=1,
  paused hook ingress=2; `BoundaryHooks` remains scalar enter/leave. The new
  ordinary entry must have an owner-validated real instruction-label witness.
  `clock_control.rs:57` currently derives a patched callback witness from a
  generated trampoline's CALL RAX; it is not an address recipe for no-patch entry.
- Pasteur must bind the actual entry/restore/return instructions and any owned
  SIGTRAP/TF correction to the same runtime boundary. No calibrated offsets,
  guessed return IP, blanket unblocking or assumption that equal RCB gives exact
  execution position. Keep selected SUD-clock refusal until this is validated.

The C helper and test branch do not require a new clock ABI. The real return
does require agreement with the evolving shared ABI; 8e's scalar types alone do
not prove that new assembly is compatible. No edits to `clock_control.rs/.s`,
`clock_boundary.rs` or Pasteur's active modules are proposed for an independent lane.

## Concrete reusable tests and actual Detcore transition

1. Reuse `InstructionTool` at `rpc_tool_guest.rs:159` and `instruction_guest`
   at :899: distinct CPUID register results, RDTSC/RDTSCP values and TSC_AUX,
   first-use runtime instruction, nested getuid and real coordinator receipt.
   Existing expectation is nine callbacks/one instruction RPC in its patched
   scenario. Retain that original case unchanged; no-patch must count repeated
   faults, not expect one initial trap and later hooks. Feature-bit masking is
   not execution interception of raw RDRAND/RDSEED.
2. Reuse straddling CPUID/RDTSC and first-use assembly sites (:616 onward),
   adding a real page-end case for each two/three-byte instruction and post-arm
   anonymous RX mappings. Snapshot live bytes and require enabled planning,
   patch/install and vDSO-rewrite counters to stay zero. Test malformed/truncated
   bytes, unsupported controls and unrelated SIGSEGV without accepting them as
   instructions or weakening the default suite.
3. Reuse the syscall fallback fixture's sentinel/redzone/XMM/errno checks, but
   **change the new instruction oracle, not the existing syscall test**: CPUID
   changes EAX/EBX/ECX/EDX; RDTSC changes EAX/EDX and preserves RCX; RDTSCP also
   changes ECX. All non-output registers/flags, actual RSP and supported FP state
   must survive a Tool callback that exercises libc/RPC. Cover occupied pending
   state and internal syscall/CPUID nesting. Inherited ptrace denial remains.
4. Preserve `tests/clocked_tool/src/lib.rs:143` (RDTSC -> sample/RPC) and :183
   (exact owned breakpoint PC/token checks). Pasteur's integrated test must
   exercise equivalent real no-patch boundaries; unchanged old fixtures alone
   do not qualify a new return path or combined clock trajectory.
5. Actual Detcore integration target stays the frozen logging DSO/harness named
   in the parent plan, not a permanent series of fake-result fixtures. Reuse
   `tool_host` result handling and shared `detcore/src/lib.rs:1276/:1328` directly.
   Retain all subscriptions, real initialization, global-clock RPC and default
   precise timer configuration. Require real instruction DETLOG/global updates,
   not RawTool constants. One additional shared-semantics check: Detcore currently
   returns `RdtscResult { aux: None }` at :1371, while the synthetic RDTSCP fixture
   supplies Some and `tool_host.rs:565` writes ECX only for Some. Resolve/test the
   established shared contract for None; do not invent host TSC_AUX or claim the
   synthetic case proves Detcore RDTSCP behavior.

Instruction continuation alone still cannot admit full Detcore: the five vDSO
subscriptions, actual timer/clock ownership and late loader/post-exec initialization
remain the exact dependencies already documented. Do not remove those guards or
disable configuration to obtain a positive result. The first eventual result is
a bounded post-install shared Detcore diagnostic, not CLI/L2/backend completion.

## Hold

No implementation was started. `8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675`
and its reviewed source remain unchanged. This note defines lane ownership and
the next test target; it neither provisions a slot nor grants activation/landing
or changes any frozen reference label.
