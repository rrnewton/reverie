# Cross-kernel trace, normalization, and acceptance matrix

Status: candidate contract for Audit A. Every row labeled **Proposed** or
**Unresolved** is not current implementation behavior.

## Observation envelope

| Trace family | Candidate fields | Comparison/normalization constraint | Status |
| --- | --- | --- | --- |
| Run identity | schema version; provider=`linux|narf`; exact kernel/Hermit/Reverie revisions; architecture; executable/dependency digests; argv/env/cwd; config/seeds; filesystem/network fixture digest | Provider and revision are provenance, not equality operands. All guest inputs/configuration must match byte-for-byte. | **Proposed** |
| Coverage receipt | subscribed syscall set and instruction-event bits; CPUID/RDTSC enablement and verification outcomes; executed event counts by family; lifecycle hooks entered; guest-memory transport and transferred byte counts; buffer-hash coverage; truncation/loss flags | Comparison must refuse if either provider omits a subscribed family, silently leaves an instruction native, cannot perform required distinct-address-space memory transfer, reports zero unexplained work, truncates, or uses a different schema. | **Proposed** |
| Event position | 1-based canonical record; scheduler turn; per-thread event ordinal; syscall pre/post phase; logical time/RCB position | Exact after normalization. Preserve all positions; never align streams by dropping an event. | **Proposed** |
| Process/thread identity | logical process/thread ID; parent; thread-group; creation mechanism; exec generation; exit/wait status | Allocate a provider-neutral identity at observed creation in deterministic tree order and preserve alias/parentage. Raw host TIDs are provenance only. | **Proposed**; mapping through nonleader exec is **Unresolved** |
| Syscall entry | ABI/arch; syscall number; six scalar args; decoded flags/enums; input byte ranges + hashes/bytes | Number and semantic scalar values exact. Pointer addresses are never compared in place of pointed-to bytes. Invalid pointer/fault behavior remains observable. | **Proposed** |
| Syscall completion | returned value or exact errno; restart class; partial count; mutable output extents/bytes/hashes; kernel-updated in/out structures; clobbered registers | Exact. Hash includes length/direction/type; for small POC fixtures retain bytes as well as hash. A hash without length/type is inadmissible. | **Proposed** |
| Persistent side effects | fd table; open-file description identity/offset/status flags; path/mapping/socket/process/signal/timer changes; later observation event | Do not infer from success. Verify with a follow-up syscall and compare both transition and observation. | **Proposed** |
| Scheduler | runnable set; selected logical TID; block reason/resource; wake source/order; yield/preemption; RCB count; syscall/signal phase | Exact under the same deterministic schedule contract. Provider transport scheduling outside guest semantics is excluded but must not reorder Tool events. | **Proposed** |
| Signal | generation source; standard/realtime; process/thread queue; `siginfo`; target; mask/disposition; dequeue order; handler entry/return; EINTR/restart/remainder | Exact Linux semantics. This is broader than the current ptrace Tool's ordinary signal-number hook. Multiple USER_NOTIF messages caused by one restarted syscall must collapse only with a verified logical-operation ID, not by adjacency. | **Proposed** |
| Time/timers | clock ID; fixed epoch; logical timestamp; duration/deadline/remainder; timer arming/expiration; CPU time; RDTSC event ordinal and Tool-selected value | Exact virtual values. Real controller wall-clock log prefixes are discarded. A subscribed RDTSC that executes natively is a coverage failure, not a value normalization. Record/replay real-time capture is not acceptable for independent cross-kernel equivalence. | **Proposed** |
| Randomness/auxv | RNG seed/stream identity; `getrandom` flags/count/error/bytes; `AT_RANDOM`; CPUID/RDRAND/RDSEED policy | Exact bytes and errors under same seed; unsupported hardware instruction must fail identically. | **Proposed** |
| Filesystem | guest path bytes; dirfd context; file kind/content digest; metadata; directory order; inode/device/mount/open-file identities; mutation result | Fixture must be immutable and identical. Guest path is compared in guest namespace, never host staging path. Identity renaming rules below apply only if explicitly enabled. | **Proposed** |
| Network/readiness | namespace/fixture; socket tuple/options/state; packet bytes/order; readiness set/order; timeout; peer-close/error | First POC excludes external network and permits controlled loopback only. No normalization of payload or readiness. | **Proposed** |
| Platform/personality | uname; auxv; page size; vDSO/vsyscall; CPUID leaf/subleaf, event ordinal, and Tool-selected register tuple; proc/sysfs bytes; supported syscalls/flags; signal numbers | Declare one virtual Linux personality and compare exact guest-visible values. A subscribed CPUID that executes natively is a coverage failure. Differences are semantic unless declared optional capability before execution. | **Proposed** |
| Memory/maps | guest mapping operation; protection/flags; file identity+offset; resulting guest address; brk; fault boundary | Host/controller addresses may ordinalize. Whether guest virtual addresses compare exactly or by mapping identity+offset must be decided before POC execution. | **Unresolved** |
| Final outputs | stdout/stderr exact bytes+length+digest; exit code or signal/core flag; final lifecycle cleanup | Exact; a deterministic nonzero exit may match, while two zero exits may diverge internally. | **Proposed** |

## Normalization rules

1. **Proposed — strip only transport wall time.** Remove the controller's real
   log prefix. Preserve virtual/logical time, counts, sizes, flags, results and
   errno exactly.
2. **Proposed — distinguish host and guest addresses.** Explicitly tagged
   controller/host addresses may become first-appearance ordinals while
   preserving identity, aliasing and order. An untagged hexadecimal number is
   not an address and must remain exact.
3. **Unresolved — guest virtual addresses.** Exact equality is the strongest
   POC rule and detects layout differences. A later equivalence-up-to-renaming
   mode may map `(mapping identity, offset)` bijectively, but only if all pointer
   comparisons, aliases, faults and printed pointer values are covered. The
   first POC should avoid needing this relaxation or require exact addresses.
4. **Proposed — logical task IDs.** Assign IDs from verified lifecycle events,
   not physical PID magnitude. Preserve parentage, thread groups and exec
   generations. Missing child/exec events make comparison `no_result`, not a
   candidate normalization.
5. **Unresolved — fd/PID/inode/port allocation.** These are guest-visible. The
   conservative POC compares raw virtualized values exactly. A later bijection
   is permissible only per identity class, with allocation order/aliasing and
   every appearance rewritten consistently. Never normalize errors caused by
   exhaustion/collision.
6. **Proposed — paths.** Compare guest byte paths after dirfd/root resolution;
   strip no guest component. Host temporary/staging prefixes never enter the
   trace. Symlink traversal and error boundary remain exact.
7. **Proposed — buffers.** Compare direction, actual moved length, and content.
   Capacity bytes not moved are excluded; in/out structs include padding only
   when Linux makes it guest-visible. Page-boundary partial transfers remain
   exact behavior.
8. **Proposed — ordering.** Do not sort events, directory entries, readiness
   arrays, messages or signals unless the Linux ABI specifies a set and the
   guest cannot observe order. Any such set normalization must be field-specific
   and versioned.
9. **Proposed — fail closed.** Invalid UTF-8 is allowed in binary fields, but
   malformed structured records, unknown schema fields, missing coverage,
   truncation and comparator errors produce `no_result`/`comparison_refused`.

## Failure and divergence classes

| Class | Falsifiable discriminator | Outcome |
| --- | --- | --- |
| Equivalent | Complete coverage receipts, exact final output/disposition, and every canonical record matches | `matched`; only this may support the POC claim |
| Pure clock | First differing semantic field is a clock/timer field; control flow and payload otherwise match | `diverged_clock` |
| Guest/control-flow divergence | Different event/syscall/scheduler prefix or count at first mismatch | `diverged_control` |
| Pure observation | Same control flow/result/length but different input/output bytes | `diverged_observation` |
| Persistent-effect divergence | Immediate completion matches but follow-up state observation differs | `diverged_state` |
| Unsupported capability | Provider explicitly rejects a required syscall/event/facility before semantic comparison | `unsupported`, never matched |
| Coverage mismatch | Subscribed event absent, lifecycle hook missing, buffer not observable, schema/provider scopes differ | `no_result: coverage` |
| Comparison refusal | Truncated/malformed/unknown trace, zero comparable work, hash artifact missing | `no_result: comparison_refused` |
| Backend/kernel fault | Panic, kernel fault, lost task, deadlock, leaked zombie, cleanup invariant failure | `infrastructure_error` with provider/stage |
| Uncontrolled input | Fixture/network/host state differs from declared input digest | `no_result: input_mismatch` unless the guest observed it, then observation divergence |

## Falsifiable acceptance primitives

These are executable test specifications, not executed evidence.

| ID | Guest/action | Required observation | Failure exposed |
| --- | --- | --- | --- |
| A0 | Static x86_64 guest `_exit(0)` | initial task start, exec boundary if loader used, one exact exit, no leaked task | boot/ELF/lifecycle baseline |
| A1 | `getpid`, invalid syscall, `getrandom` into known buffer | one syscall pre/post each; exact logical PID, ENOSYS policy, byte count/content | seccomp event and result/buffer path |
| A2 | `read`/`write` with buffer crossing into `PROT_NONE` | Linux-compatible partial count/EFAULT and exactly moved bytes | process-vm/ptrace memory fault semantics |
| A3 | syscall replacement plus injected `openat`/`close` | original suppressed once; injected call args/result; full register context restored | GET/SETREGSET, SINGLESTEP, SYSCALL/TRACESYSGOOD |
| A4 | pthread clone, process fork, vfork+exec | child identity, parentage, Tool start, vfork suspension, exec generation, wait status | ignored lifecycle options/GETEVENTMSG |
| A5 | nonleader thread calls exec | former TID maps to group leader exactly; sibling exits and state migrates once | EXEC event/GETEVENTMSG/de-thread |
| A6 | robust mutex owner exits with clear-tid waiter | owner-word transition, futex wake, waiter result/order | exit hooks, robust-list ordering |
| A7 | queued realtime signal with value; standard signal duplicates; blocked/unblocked | exact `siginfo`, coalescing/queue order, chosen recipient, mask/disposition | GETSIGINFO and signal model |
| A8 | interrupt read/poll/nanosleep with and without SA_RESTART | exact EINTR/restart, partial bytes and remaining timeout; one logical operation | restart semantics and USER_NOTIF duplication |
| A9 | two runnable threads + futex + deterministic PMU preemption | same runnable choices, blocks/wakes, RCB position, virtual time | scheduler/perf/timer contract |
| A10 | clock APIs, timerfd, RDTSC/CPUID | fixed epoch/logical progression and exact expiration/order | time/hardware personality |
| A11 | open/dup/fork/read/write/stat/getdents/procfs on immutable fixture | exact bytes, offsets, descriptor aliases, metadata/ordering, persistent mutation | filesystem and identity model |
| A12 | loopback socketpair/TCP, poll/epoll, partial send/receive | exact packet bytes, readiness/order, socket state, timeouts | network/readiness semantics |
| A13 | uname, auxv, `/proc/self/status`, vDSO clock | declared virtual personality bytes exactly match | hidden platform inputs |
| A14 | kill supervisor mid-run and force Tool error | EXITKILL-equivalent containment; bounded cleanup; no escaped process/zombie | security/lifecycle cleanup |
| A15 | negative control mutates one return, errno, buffer byte, schedule turn, signal payload, and output byte in separate trials | comparator reports the correct first divergent record/class every time | comparator sensitivity/no goalpost weakening |
| A16 | remove one event family and truncate one side in separate trials | comparator refuses; never reports divergence or match | coverage/truncation fail-closed gate |
| A17 | from the external tracer, use `MemoryAccess` to read and rewrite a known 32-byte tracee buffer | exact 32-byte read and write succeed across distinct address spaces; receipt names `process_vm_readv/writev`, target logical TID, byte count and digest | current Narf distinct-AS `EPERM`; stock bulk data movement prerequisite |
| A18 | under default fail-closed Detcore, execute one `rdtsc` and one `cpuid` leaf/subleaf with native values chosen to differ from Tool results | subscription receipt has both bits; exactly one RDTSC and one CPUID Tool event; guest observes only Tool-selected value/register tuple; setup results prove `PR_TSC_SIGSEGV` and `ARCH_SET_CPUID(0)` effective | accepted-but-ignored TSC mode, missing CPUID faulting, or silent native execution |

Acceptance requires paired Linux and Narf traces from the same fixture digest,
plus naked native baselines where useful. Each cell records exact command,
revisions, backend, architecture, seeds, capabilities, duration, exit status,
trace counts and artifact hashes. A test that runs only on Linux, asserts only
exit 0, or is skipped for Narf is not cross-kernel evidence.
