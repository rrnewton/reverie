# Audit A — correctness and source-coverage gate

## Verdict

**CHANGES REQUIRED.** The current evidence set is not locked and Audit B must
not begin. Three load-bearing current-state facts are wrong or incorrectly left
unresolved, and the tracked coverage record does not satisfy the required
heading/provenance controls. The corrections are bounded and do not invalidate
the useful Narf, Hermit, Linux, or trace-contract work described below.

Review target:

- parent `dev-hermit`: `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`;
- `rrnewton/hermit`: [`ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2);
- Hermit's build-time Reverie pin: [`ae1d1da78d2d89a1fa0454ee2282cf135e2286ed`](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed);
- separately inventoried `rrnewton/reverie`: [`123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937);
- Narf: [`hodgesds/narf@afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/tree/afe919924a5fd2c8e820ab3a0564b6d45438e6bc);
- Linux man-pages: `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`.

All thirteen pre-existing durable evidence files, including all 52 JSONL
records, were read. Every JSON line parses with `jq`. The Narf clone and all
three product revisions above were independently reconciled with `git`.

## Findings requiring correction

### Severity 0 — LiteInst's public `Backend` path is misclassified

`reverie-facts.md:120`, `reverie-backend-matrix.md:13-14,31-32`, and JSONL
finding `R-F14` say that the conforming/public `LiteinstBackend` path is the
ptrace-owned hybrid and that the direct path has no `Backend` implementation.
The exact source says the opposite:

- [`LiteinstBackend::run_with_preload`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L361-L393)
  is the in-guest/UDS path and explicitly says it does not attach ptrace;
- [`impl Backend for LiteinstBackend`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L590-L645)
  calls `run_with_preload*`, so the trait implementation is that direct path;
- [`run_host_with_preload`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L204-L238)
  is the separate ptrace-owned hybrid API;
- pinned Hermit deliberately calls that separate hybrid API at
  [`hermit-cli/src/lib.rs`](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L2756-L2771).

Impact: the provider matrix currently assigns ptrace lifecycle guarantees to a
public trait implementation that does not obtain them. Required correction:
split “LiteInst trait/direct” from “LiteInst ptrace-host hybrid,” mark the former
as the `Backend` implementation with its direct lifecycle limits, and describe
Hermit's CLI choice as an explicit call to the latter. Correct or replace
`R-F14` rather than preserving the false combined claim.

### Severity 0 — Narf `process_vm_*` is known-incompatible, not unresolved

`hermit-facts.md:138-141`, `reverie-facts.md:149`, and the ptrace matrix call
Narf's `process_vm_readv/writev` status unresolved. The Narf coverage ledger
says the implementation was opened, but its load-bearing result was not
carried into the fact set. At the pinned commit,
[`process_vm_transfer`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6880-L7038)
supports only self or `CLONE_VM`/same-address-space transfers and returns
`EPERM` for a target in another address space. Stock Reverie uses
[`process_vm_readv/writev`](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/safeptrace/src/memory.rs#L62-L125)
for bulk access from the tracer process to the distinct tracee process.

Impact: stock `MemoryAccess` cannot work for bulk cross-process data on current
Narf even if the ptrace-event gaps were fixed. Required correction: promote
this to **Verified current (source-only), fatal for the stock out-of-process
bulk-memory path** and add it to the JSONL and option/provider matrices. Keep
page-boundary partial-transfer behavior separately unresolved; the current
implementation gathers before scattering and returns `EFAULT` rather than a
Linux partial byte count when a later segment faults.

### Severity 0 — default instruction interception requirements are omitted

The existing “exact ptrace” inventory treats `PR_SET_TSC`, perf, and Narf's
instruction-trap behavior as unresolved and does not inventory CPUID faulting.
This omits a default Detcore dependency:

- with the default fail-closed configuration, pinned Detcore returns
  [`Subscription::all()`](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1077-L1090);
- Reverie's `Subscription::all()` includes both CPUID and RDTSC
  ([source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/subscription.rs#L35-L44));
- Reverie requests `PR_SET_TSC(PR_TSC_SIGSEGV)` before exec
  ([source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/tracer.rs#L1638-L1650))
  and later uses `ARCH_SET_CPUID(0)` for CPUID faulting;
- Narf accepts `PR_TSC_SIGSEGV` but explicitly never disables RDTSC
  ([source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L407-L431));
- Narf's `arch_prctl` implements FS/GS operations only and returns `EINVAL` for
  other subcodes, including `ARCH_SET_CPUID`
  ([source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L5-L83)).

Impact: a default strict Hermit run would not receive RDTSC or CPUID Tool events
on current Narf. Required correction: add both mechanisms to the exact-required
surface and classify them as **Verified current source gaps**. A deliberately
reduced POC may disable these subscriptions only if the claim scope and guest
exclude the instructions; that is a proposed narrower experiment, not stock
compatibility.

### Severity 1 — coverage and structured-finding controls are incomplete

1. The protocol requires headings for every opened document. The ledgers give
   useful symbol/heading summaries, but `narf-source-coverage.md` records
   selectively reading `STATUS.md`, `epoll_hang.md`, research summaries, and
   other excluded documents without recording their headings in the tracked
   ledger. Provider READMEs are also summarized by “scope” rather than a
   complete heading list. Required correction: record headings for every opened
   document or state that it was discovered but not opened.
2. The JSONL is syntactically valid and every file reference has an existing
   local file/range, but it is not strictly valid against the skill's canonical
   finding schema: zero of 52 IDs matches `F-N`, and 36 `file_ref.snippet`
   values contain ellipses rather than literal source text. Required correction:
   either normalize the records to the schema or explicitly version/document
   the repository-specific deviation; make snippets literal so provenance is
   machine-reproducible.
3. The Hermit query pairs are recorded in prose but, unlike Narf and Reverie,
   their raw results are not preserved under the ignored evidence directory.
   This does not refute the source facts, but the claim that the search is
   reproducible is weaker. Preserve command/result receipts or rerun and record
   them.

## Canonical fact set independently reproduced

These facts are supported despite the findings above. They are not a LOCKED
authorization for Audit B until the severity-0 contradictions are corrected.

1. **Verified current:** pinned Hermit builds Reverie `ae1d1da…`, seven commits
   after the parent repository's separate Reverie pin `123df7c…`. The distinction
   must remain explicit; actual Hermit execution facts use `ae1d1da…`.
2. **Verified current:** Reverie's backend-neutral boundary is
   `Backend`/`Tool`/`Guest`/`GlobalTool`/`Subscription`/`MemoryAccess`. A complete
   backend owes arbitrary-Tool hosting, whole-tree lifecycle, subscribed event
   routing, state initialization/teardown, real output, and real statistics.
3. **Verified current:** exactly three source implementations of the public
   `Backend` trait exist at `123df7c…`: ptrace, LiteInst, and e9patch. This count
   excludes the generic `IntoGuest` adapter and does not imply runtime passes.
4. **Verified current:** stock ptrace launches with TRACEME/SIGSTOP, requests
   EXEC/EXITKILL/CLONE/FORK/VFORK/VFORKDONE/EXIT/SECCOMP/TRACESYSGOOD together,
   uses selective seccomp `RET_TRACE` for syscall prehooks, and requires the
   event/status/register/memory/signal semantics enumerated below.
5. **Verified current:** Narf ptrace is real, not a stub. Its x86_64 syscall-stop,
   basic register, word-memory, attach and resume fixtures have exact-commit QEMU
   evidence. It is nevertheless not stock-Reverie-compatible: lifecycle/seccomp
   events and event messages are absent, `GETSIGINFO` is absent, most accepted
   option bits are inert, stopped-state validation and some signal semantics
   diverge, bulk cross-AS process-vm access is rejected, and default CPUID/RDTSC
   interception is absent.
6. **Verified current:** Narf seccomp filter mode is stored/reported but filters
   are not evaluated; `NEW_LISTENER` is rejected and there is no notification
   object/ioctl lifecycle. Linux user notification is therefore unavailable on
   current Narf.
7. **Verified current:** Narf has no generic typed syscall-interposition trait.
   The live syscall dispatcher contains concrete ptrace entry/exit calls and a
   textual feature-gated logger.
8. **Verified current:** Narf has a substantial BPF runtime, including verified
   programs, maps, fd lifetime, atomic/sleepable execution, and domain guards.
   `BPF_PROG_TYPE_SYSCALL` can load/test-run, but no syscall attach target or
   syscall trace-context ABI invokes it on guest syscalls.
9. **Verified current:** domain enforcement is conditional on the selected
   backend and mapping/tag coverage; FRAME/domain 0 remains accessible. Hardware
   domains are defense in depth, not a complete sandbox for an interposer.
10. **Verified current:** KVM, DBT, preload, SaBRe, and user-notification code are
    real provider/helper paths but do not implement the generic `Backend` trait.
    Neither a crate name, test, nor callback registration is runtime proof.
11. **Verified current:** e9patch's public `Backend` is ptrace-hosted; its direct
    path is separate and lifecycle-incomplete. LiteInst's public `Backend` is
    direct/in-guest; its ptrace-host hybrid is a separate API used by Hermit.
12. **Verified current:** current strict Hermit canonical comparison removes
    only wall-clock log prefix and canonicalizes marked host addresses; it
    preserves substantive values and requires nontruncated structured records
    and I/O-buffer evidence. Record/replay verifies one recording's replay and
    does not establish equality of independent Linux and Narf executions.
13. **Proposed:** every row of `trace-input-matrix.md`, including normalization,
    failure classes, coverage receipts, and A0-A16 acceptance primitives, is a
    candidate cross-kernel contract. None is current Hermit-on-Narf runtime
    support.
14. **Above the API:** identical executable/dependency/filesystem/network inputs
    and their digests are harness obligations. A backend trait does not create
    equivalent external worlds.
15. **Deferred:** external networking, performance equivalence, arbitrary
    guest-address renaming, and production hardening remain outside the first
    functional POC.

## Complete current provider matrix (corrected)

| Provider/mechanism | Public `Backend` | Placement/lifecycle fact | Current evidence class |
|---|---|---|---|
| ptrace | Yes | External tracer; complete contract is implemented through Linux ptrace/seccomp state machine | **Verified current source**; no run in this research |
| KVM | No | Host VMM plus bounded x86_64 bare Linux-like personality; not a Narf guest-kernel transport | **Optional capability / Software helper** |
| DynamoRIO DBT | No | In-process translated client with syscall/signal/thread callbacks; x32 and `int 0x80` rejected | **Optional capability / Software helper** |
| preload | No | Trusted dynamic x86_64 DSO using seccomp TRAP/SIGSYS; no full loader/static/vDSO/exec lifecycle | **Software helper** |
| LiteInst trait/direct | Yes | In-guest patching plus UDS coordinator; process-like fork/clone3 and translated vfork, but thread clone/exec/general signal/timer completeness absent | **Verified current source, bounded backend** |
| LiteInst ptrace-host hybrid | No separate trait impl | Separate `run_host_with_preload*`; ptrace owns lifecycle/tool and hot sites use LiteInst; this is Hermit's CLI path | **Optional current path** |
| e9patch hybrid | Yes | Root AOT rewrite, ptrace lifecycle/residual coverage, ptrace fallback for non-ELF | **Verified current source, bounded backend** |
| e9patch direct | No separate trait impl | In-process AOT/preload/UDS path with incomplete lifecycle | **Optional capability** |
| SaBRe + reverie-host | No | In-process loader/plugin plus host RPC; adapter implements Guest, not Backend | **Software helper / experimental adapter** |
| seccomp user notification | No | External listener helper supports receive/send/id-valid/addfd but no Tool/Guest/lifecycle backend | **Software helper** |
| riptrace | No | Frontend over other runtime pieces | **Seen and excluded** |

## Current-Narf interposition-option matrix

| Option | Stop/callback and lifecycle | Registers/memory/data movement | Isolation/concurrency | Current-Narf fact and change boundary |
|---|---|---|---|---|
| Stock ptrace | Requires seccomp event prehooks plus fork/clone/vfork/exec/exit events and wait encoding | GPR/FP/XSTATE, signal info, PEEK/POKE, cross-AS process-vm bulk, injection/single-step | External tracer; per-task stops but complete SMP stopped-state semantics required | **Not viable unchanged.** Multiple fatal current gaps above |
| Seccomp user notification | Pre-syscall notification only; no inherent exec/child/exit/signal/timer model | Six scalar args; response/continue/addfd; target-memory access is separate and racy | External supervisor; cancellation/restart/TOCTOU rules required | **Absent on current Narf.** Requires filter evaluator, listener fd, ioctls, task cancellation/lifecycle, and memory policy |
| Ptrace-compatible emulation | Would reproduce the stock wait/option/event ABI rather than change Reverie | Must add all stock event, regset, signal, process-vm, TSC/CPUID and tracer-death semantics | Retains external-tracer trust/cost and must synchronize stopped tasks on SMP | **Proposed**, large compatibility project; current ptrace name/subset is insufficient |
| First-class in-kernel Rust interposition | New typed pre/post/lifecycle callbacks at the live dispatcher | Kernel can provide validated scalar context plus bounded copy helpers/capability-scoped memory handles | Must define registration, ownership, ordering, reentrancy, blocking, teardown and authorization | **Proposed**; no current trait. Concrete ptrace hooks are the nearest insertion seam |
| Framekernel/BPF-like extension | New syscall entry/exit attach targets and task lifecycle linkage | Copied typed context, maps/ring output, and mediated copy helpers; no raw arbitrary kernel pointers | Verifier is primary boundary; domain guard is defense in depth and FRAME remains reachable | **Proposed on existing ingredients**; current syscall program type is test-run only and has no attach path |

## Severity-ranked contradiction ledger

| ID | Severity | Conflict | Canonical disposition |
|---|---:|---|---|
| A-C1 | 0 | Evidence calls LiteInst trait path hybrid; source trait calls direct path | Source wins; correct provider matrix and R-F14 |
| A-C2 | 0 | Narf process-vm listed unresolved; implementation is same-AS-only | Source wins; stock external bulk memory is incompatible |
| A-C3 | 0 | Default instruction events omitted; Narf accepts-but-ignores TSC faulting and lacks CPUID faulting | Source wins; add current fatal semantic gaps |
| N-C1 | 0 | Narf docs describe `linux-compat` feature; Cargo/source have none | ABI is always present at this pin; docs stale |
| N-C2 | 0 | Narf docs claim SEIZE; exhaustive request source has no SEIZE | Ptrace is live, SEIZE absent |
| N-C3 | 1 | Ptrace “stub” comments survive beside live implementation | Live dispatcher/source and executed fixtures prevail |
| N-C4 | 1 | MTE comments disagree with implementation and allocation coverage | Save/TCF implementation verified; coverage/hardware runtime unresolved |
| N-C5 | 1 | Module-domain docs lag live conditional entry/exit | Conditional implementation verified; hardware breadth unresolved |
| N-C6 | 1 | PCID described wired but unexecuted on available runners | Source-only, never demonstrated |
| N-C7 | 1 | Broad compatibility prose conflicts with failed jobs and interface gaps | Per-interface source/per-job evidence only |
| R-C1 | 1 | `BACKENDS.md` links old `2f812…` source | Discovery only; exact pinned implementation wins |
| R-C2 | 1 | Provider READMEs lag DBT/LiteInst/e9patch/SaBRe implementation | Current implementation wins; runtime remains unexecuted |
| N-C8 | 2 | Module docs disagree about real `.ko` tests | Loader source implemented; real-module execution unresolved |

## Unresolved decisions and missing evidence

- No external Hermit, strace, or gdb tracer ran against Narf; no
  Hermit-on-Narf runtime exists.
- Narf `PTRACE_SINGLESTEP`, syscall restart, wait ordering, realtime signal
  reinjection, FP/XSTATE, tracer-death cleanup, and SMP stop synchronization are
  not independently demonstrated. Some have known source gaps.
- Narf perf has substantial source, but its exact raw retired-conditional-branch
  event, overflow signal, mmap/rdpmc, ioctl-period, skid, and `GETSIGINFO`
  behavior has not been matched to Reverie's timer contract. Missing
  `GETSIGINFO` already prevents the stock timer path.
- Intended POC hardware/domain backend and actually executed enforcement are
  unspecified. PCID/MTE breadth is not runtime-proven.
- Guest virtual-address equality versus a verified mapping bijection, and later
  PID/fd/inode/port renaming, must be fixed before execution.
- Cross-provider event membership and canonical total ordering are not defined
  by Reverie; the proposed coverage receipt must be implemented and tested.
- The pinned Narf CI snapshot was collected while the workflow was in progress.
  Individual job conclusions and log lines are exact-commit evidence, not a
  timeless claim about later reruns.
- Public line links are commit-pinned except repository-identity roots and
  action-run/job URLs. The local exact revisions were the byte authority; no
  browser link-resolution test was performed.

## Facts Audit B may not reinterpret

Audit B must not start until this audit is rerun to **LOCKED**. When it does,
the following constraints are immutable unless new primary evidence is added:

1. “Narf has no ptrace” is false; “current Narf supports stock Reverie ptrace”
   is also false.
2. LiteInst's trait/direct path and Hermit's selected ptrace-host hybrid are
   distinct paths.
3. Missing event stops/messages, cross-AS bulk memory, signal provenance,
   CPUID/RDTSC interception, and cleanup semantics cannot be hidden behind the
   presence of basic syscall stops.
4. Seccomp mode/action names do not prove filter or user-notification support.
5. A BPF syscall program type does not prove a syscall attach point.
6. Hardware domains do not replace verifier/typed-helper safety and do not
   protect FRAME/domain-0 memory.
7. Return/errno equality without exact moved data and persistent side effects
   is not functional equivalence.
8. Record/replay fidelity is not independent cross-kernel equivalence.
9. Proposed trace/normalization/acceptance rules are not implemented runtime
   support.
10. Test declarations, source names, comments, and accepted-but-inert options
    are not execution evidence.

## Denominator check

| Figure | Denominator and comparability judgment |
|---|---|
| 2,956 Narf paths | Tracked paths at the pinned commit, not files read end-to-end and not capabilities |
| Narf query hit counts | Matching output lines, explicitly not unique files/capabilities; precise and naive counts are discovery controls, not comparable support metrics |
| 3 `Backend` implementations | All Rust `impl Backend for` matches at `123df7c…`; excludes inherent APIs/helpers |
| 6 execution-specific `Guest` implementations | Concrete provider guests only; excludes generic `IntoGuest`, so it must not be compared as “all Guest impls” without that qualifier |
| 16 domain IDs | Entire reserved `DomainId` namespace, not sixteen enforcement domains demonstrated at runtime |
| Reverie provider test-file/ignore counts | Matching source files and annotations respectively; not executed cases or pass rates |
| Narf CI pass statements | Named visible `[ OK ]` log cases or individual GitHub job conclusions; the whole x86_64 kernel-test job failed after 457 seconds |
| A0-A16 | Seventeen proposed acceptance IDs; zero executed cross-kernel cells |

No current-support percentage or provider success rate is admissible from these
figures. Historical I/O/performance percentages found in comments were not used
as current cross-kernel evidence.

## Runtime/source reconciliation

The auditor executed **no runtime test** and no Hermit invocation. The research
set contains exact-commit remote Narf CI runtime evidence: named x86_64 ptrace
fixtures visibly passed in GitHub Actions job `107842987657`, and the same job
later failed when QEMU exited with SIGSEGV after 457 seconds. Other cited job
conclusions came from the exact SHA's captured check-run JSON. Reverie/Hermit
provider claims are source-only; no backend run was performed. No source-only
claim has been promoted to executed support in this audit.

## Goalpost-moving assessment

- Assertions weakened: **no**. No production/test assertion was changed in the
  reviewed untracked report set.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. The proposed comparator preserves exact fields, refuses missing or
  truncated coverage, and includes negative controls A15/A16.
- Failure renamed or relabelled as a pass: **no**. Unsupported, no-result,
  divergence, and infrastructure failure remain distinct from `matched`.
- Check deleted instead of satisfied: **no**. The completion checklist remains
  visibly open. This audit refuses lock rather than treating an unchecked item
  or unresolved runtime as success.

This assessment is bound to the exact current untracked evidence tree reviewed
on 2026-09-24. A correction changes the audit head and requires a fresh Audit A
lock before reader-facing drafting.

## Verification completion

- `git diff --check` completed with exit status 0 and no output.
- Because this audit file is new/untracked, it is invisible to the ordinary
  command above; `git diff --no-index --check /dev/null evidence/audit-a.md`
  produced no whitespace-error output. Its exit status 1 is the documented
  no-index “files differ” result, not a check failure.
- `jq -c .` succeeded for all three JSONL files (52 records total).
- Every JSONL `file_ref` path exists at its mapped exact-revision checkout and
  every cited inclusive line range is within that file.
