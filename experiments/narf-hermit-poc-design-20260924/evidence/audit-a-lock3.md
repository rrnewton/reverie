# Audit A final correctness lock

## Verdict

**LOCKED**

The canonical fact set is internally consistent at the reviewed evidence head.
Every previously blocking contradiction is closed, the source-coverage controls
are complete enough for this design decision, and current behavior remains
separate from proposed behavior and executed evidence. Audit B may begin using
the immutable facts in this report.

This was a fresh, read-only correctness audit. The only file written by this
auditor is this report. No Hermit, Narf guest, QEMU guest, or provider runtime
was executed.

## Review target and source identity

- Registered slot: `narf-hermit-design`, generation 1, machine `devbig030`,
  branch `codex/narf-hermit-design`, purpose `Research and design
  Hermit-on-Narf POC`. `wrkslots status` reported consistent registry storage,
  the requested checkout path, live owner PID 3869859, and start point
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`.
- Parent `dev-hermit` start:
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`.
- [`rrnewton/hermit@ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2).
- Hermit's build-time
  [`rrnewton/reverie@ae1d1da78d2d89a1fa0454ee2282cf135e2286ed`](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed).
- Separately inventoried
  [`rrnewton/reverie@123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937).
- [`hodgesds/narf@afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/tree/afe919924a5fd2c8e820ab3a0564b6d45438e6bc).
- Linux man-pages:
  [`227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/commit/?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7).

All five local source trees were clean and matched those revisions.
`ae1d1da…` is seven commits after `123df7c…`; Hermit execution-path facts use
its actual build pin. The standalone Reverie pin remains the authority for the
provider inventory.

## Findings

No open severity-0, severity-1, or severity-2 correctness finding remains in
the current fact/coverage/matrix evidence. Historical `CHANGES REQUIRED`
reports remain in the evidence tree as the correction record; they are not the
current verdict.

## Requested closure checks

### F-C1 and A2-C1 — current Narf seccomp status: closed

Every current non-audit evidence file now uses the same disposition:

- **Verified current (source-only):** Narf's complete `sys_seccomp` handler
  stores mode `2` for strict/filter requests but does not copy, retain, or
  evaluate a filter. Recognizing action values through
  `SECCOMP_GET_ACTION_AVAIL` does not execute those actions.
- **Verified current (source-only):**
  `SECCOMP_FILTER_FLAG_NEW_LISTENER` is rejected with `EINVAL`, and the exact
  tree has no user-notification object, receive/send/addfd ioctl lifecycle, or
  syscall-dispatch filter evaluator.
- **Unresolved:** restart, cancellation, target-memory movement, listener
  lifetime, and ioctl behavior for a *proposed future* Narf notifier.

This closes both names used by earlier audits for the same contradiction.
`narf-facts.md`, `hermit-facts.md`, `reverie-facts.md`, both relevant coverage
ledgers, the backend matrix, and finding `N-F-12` no longer demote the known
current negative to an unknown. The correction remains source-only; it is not
reported as an executed runtime result or as proof that Narf cannot add the
feature later.

### F-C2 — lane schema counts and nonliteral forms: closed

A fresh exact-revision byte comparison reproduced the complete preliminary
lane denominator:

- 61 records: 23 Narf + 18 Reverie + 20 Hermit;
- 61 unique IDs and no unresolved dependency ID;
- 99 `file_ref` records, all resolving to an existing exact revision, path,
  and inclusive line range;
- 69 literal contiguous snippets;
- 24 snippets containing an explicit ellipsis;
- 2 whitespace-compacted snippets: `N-F-3` and `N-F-5`;
- 4 unmarked omission or Markdown-normalization snippets: `N-F-1`, `N-F-12`,
  `N-F-13`, and `N-F-20`.

`finding-schema-notes.md` accurately names the shared
`narf-hermit-research-lane/v1` profile, describes all three nonliteral forms,
names the four third-form records, and forbids canonical verification of every
nonliteral preliminary receipt. The Hermit ledger's narrower
`hermit-evidence/v1` description is compatible with the shared profile: that
shard's snippets are literal and its only canonical-schema deviation is its
namespaced ID. The shared profile remains authoritative for the three-file
research corpus.

The final trust gate is not weakened. Reader-facing prose may cite only a new
`deep-research-finding-schema/v1` stream with unique `F-N` IDs, literal exact
snippets, and independent `verification.jsonl` verdicts. None of these 61
preliminary records has been promoted to final verified input.

### All earlier blockers: closed

| Historical item | Closure reproduced from pinned primary source |
|---|---|
| A-C1 / L-C1 — LiteInst path inversion | `impl Backend for LiteinstBackend` calls direct/in-guest `run_with_preload*`; `run_host_with_preload*` is the separate ptrace-host hybrid explicitly selected by Hermit. |
| A-C2 / L-C2 — process-vm classification | Narf permits self/same-address-space transfers and returns `EPERM` for a distinct address space. Stock external Reverie uses `process_vm_readv/writev` against its distinct tracee, so its bulk path is incompatible. Narf's gather/scatter behavior is source-verified; exact paired Linux page/segment outcomes remain **Unresolved**. |
| A-C3 — default instruction interception | Default fail-closed Detcore returns `Subscription::all()`, including CPUID and RDTSC. Reverie requests TSC faulting and injects `ARCH_SET_CPUID(0)`; Narf accepts-but-ignores the former and lacks the latter. |
| L-C3 — Reverie query receipts | R-Q1–R-Q14 each has a focused and exact two-word naive query with distinct command/stdout/stderr/exit receipts. The 115-entry integrity manifest verifies with no mismatch. |
| L-C4 — preliminary schema profile | The common profile, complete form inventory, and separate canonical trust-gate boundary are explicit. |
| F-C1 / A2-C1 — seccomp demotion | Current filter execution and listener support are absent/inert by source; only future notifier semantics remain unresolved. |
| F-C2 — omitted snippet forms | The reproduced 69/24/2/4 classification matches the shared profile and no preliminary nonliteral record is eligible for canonical verification. |

## Locked canonical fact set

1. **Verified current:** the backend-neutral Reverie contract is
   `Backend`/`Tool`/`Guest`/`GlobalTool`/`Subscription`/`MemoryAccess`. A
   conforming implementation owes arbitrary-Tool hosting, one global state,
   whole process/thread-tree lifecycle, all subscribed events, guest mutation
   and data movement, exit hooks, output capture, and real activity statistics.
2. **Verified current:** exactly three production blocks implement Reverie's
   public `Backend` trait at `123df7c…`: ptrace, LiteInst, and e9patch. KVM,
   DBT, preload, SaBRe/host, and seccomp user notification are implemented
   narrower providers/helpers, not additional public trait implementations.
3. **Verified current:** LiteInst's public trait path is direct/in-guest and
   bounded. Its ptrace-owned hybrid is a separate API selected by Hermit.
   E9patch's public trait path is a ptrace-hosted rewrite hybrid; its direct
   path is separate and lifecycle-incomplete.
4. **Verified current:** stock ptrace launches with TRACEME and an initial
   stop, requests EXEC/EXITKILL/CLONE/FORK/VFORK/VFORKDONE/EXIT/SECCOMP/
   TRACESYSGOOD together, and uses selective seccomp `RET_TRACE` for ordinary
   subscribed-syscall prehooks.
5. **Verified current:** the stock ptrace state machine depends on Linux event
   status encoding, `GETEVENTMSG`, `GETSIGINFO`, GPR/FP/XSTATE regsets,
   PEEK/POKE, distinct-address-space `process_vm_*`, SINGLESTEP/SYSCALL resume,
   signal reinjection/provenance, and tracer-death containment. Basic syscall
   entry/exit stops alone are insufficient.
6. **Verified current:** Narf has real ptrace, not a stub. Its exact-commit
   x86_64 QEMU log contains named basic ptrace cases and two live process
   ptrace smokes marked `[ OK ]`. That evidence is narrower than an external
   tracer run, and the same job later failed when QEMU exited with SIGSEGV
   after 457 seconds.
7. **Verified current:** current Narf is not compatible with stock Reverie
   ptrace. Lifecycle/seccomp events and event messages are absent,
   `GETSIGINFO` is absent, only TRACESYSGOOD acts among the stored option bits,
   stopped-state and signal semantics are incomplete, distinct-AS bulk memory
   is rejected, and default CPUID/RDTSC delivery is absent or inert.
8. **Verified current:** Narf filter execution and user notification are
   absent/inert by pinned source as stated in the F-C1 closure. Linux Reverie
   does contain a reusable notifier helper, but no current Reverie `Backend`
   consumes it as its execution mechanism.
9. **Verified current:** Narf's live syscall dispatcher contains concrete
   ptrace entry/exit calls and optional textual logging, but no generic typed
   Rust syscall-interposition trait.
10. **Verified current:** Narf has verifier, JIT/interpreter, maps, fd
    lifetimes, atomic/sleepable BPF execution, and domain guards.
    `BPF_PROG_TYPE_SYSCALL` loads and test-runs, but the attach resolver exposes
    fentry and XDP only; there is no guest-syscall attach point or syscall
    trace-context/copy ABI.
11. **Verified current:** Narf hardware-domain enforcement is conditional on
    the selected backend and mapping/tag coverage. FRAME/domain 0 remains
    writable while BPF executes, so verifier, interpreter/JIT, typed helpers,
    dispatcher, and domain-manager correctness remain trusted. Domains are
    defense in depth, not a complete interposer sandbox.
12. **Verified current:** Narf currently schedules from per-CPU queues with
    optional work stealing and exposes hardware-derived time, mutable VFS and
    network state, CSPRNG output, signals, PIDs/fds, and address layout. No
    Hermit-style deterministic global scheduling, virtualized-time, or
    record/replay input-capture implementation was found in Narf.
13. **Verified current:** strict Hermit comparison preserves substantive
    values, requires complete structured records and I/O-buffer evidence, and
    distinguishes match/divergence/no-result/infrastructure error from guest
    exit status. Current record/replay proves fidelity to one recording, not
    equality of independent Linux and Narf executions.
14. **Proposed:** every trace field, normalization, coverage receipt, failure
    class, and A0–A18 case in `trace-input-matrix.md` is a candidate
    cross-kernel contract. None is current Hermit-on-Narf runtime support.
15. **Above the API:** byte-identical executable/dependency/argv/environment,
    an equivalent guest-visible filesystem, controlled network inputs, and
    their digests are harness obligations; the Reverie trait does not create
    equivalent external worlds.
16. **Deferred:** uncontrolled external networking, performance equivalence,
    arbitrary identity/address renaming, and production hardening remain
    outside the smallest functional POC.

## Complete current provider matrix

| Provider/mechanism | Public Reverie `Backend`? | Locked disposition |
|---|---:|---|
| ptrace | Yes | External Linux ptrace/seccomp reference path; source-inspected, not run in this research |
| KVM | No | Bounded x86_64 bare Linux-like personality; **Optional capability / Software helper** |
| DynamoRIO DBT | No | In-process translated provider with syscall/signal/thread/instruction callbacks; **Optional capability / Software helper** |
| preload | No | Trusted dynamic x86_64 seccomp-TRAP/SIGSYS primitive with explicit loader/static/exec gaps; **Software helper** |
| LiteInst trait/direct | Yes | Bounded direct in-guest backend; no ptrace lifecycle inheritance |
| LiteInst ptrace-host hybrid | No separate impl | Separate optional API selected by Hermit; ptrace owns lifecycle/control |
| e9patch hybrid | Yes | Root rewrite plus ptrace lifecycle and residual coverage |
| e9patch direct | No separate impl | Optional in-process path with incomplete lifecycle |
| SaBRe + reverie-host | No | Experimental in-process adapter plus host RPC launcher; **Software helper** |
| seccomp user notification | No | Reusable Linux listener/ioctl helper with no current execution-backend consumer |
| `riptrace` | No | Front end over other mechanisms; excluded as a provider/backend |

The row count is eleven mechanisms after splitting direct/hybrid paths and
including an excluded front end. It is not a count of conforming Hermit
backends.

## Complete current-Narf interposition-option matrix

| Option | Locked current-state disposition |
|---|---|
| Stock ptrace | **Not viable unchanged.** Several independently fatal event, memory, instruction, signal, and lifecycle gaps exist. |
| Seccomp user notification | **Verified current source-only absent/inert.** Filter execution and listener/ioctl lifecycle do not exist. |
| Ptrace-compatible emulation | **Proposed.** It is a large Linux-compatibility project covering events/messages, regsets, memory, signals, instruction traps, SMP stops, and cleanup. |
| First-class in-kernel Rust interposition | **Proposed.** The live dispatcher is the nearest seam; ownership, lifecycle, ordering, reentrancy, blocking, memory authority, authorization, and teardown remain design obligations. |
| Framekernel/BPF-like extension | **Proposed on existing ingredients.** It needs syscall entry/exit attach targets plus a typed context and bounded copy ABI; the verifier is primary isolation and domains are defense in depth. |

## Contradiction ledger

| ID | Severity | Conflict | Locked disposition |
|---|---:|---|---|
| A-C1 / L-C1 | 0 | Earlier evidence inverted LiteInst direct and hybrid paths | Closed: public trait is direct; Hermit explicitly selects the separate ptrace-host hybrid |
| A-C2 / L-C2 | 0 | Earlier evidence left all Narf process-vm behavior unresolved or called every boundary a proved Linux divergence | Closed: distinct-AS `EPERM` and stock impact are verified; Narf gather/scatter is verified; exact paired Linux boundary results remain unresolved |
| A-C3 | 0 | Default CPUID/RDTSC requirements were omitted | Closed: requirements and current Narf gaps are explicit |
| F-C1 / A2-C1 | 0 | Current Narf seccomp/filter/notifier absence was called unresolved | Closed: current support is absent/inert by source; only future contract semantics are unresolved |
| N-C1 | 0 | Narf docs describe a `linux-compat` feature absent from Cargo/source | Linux ABI is always present at this pin; feature prose is stale |
| N-C2 | 0 | Narf docs claim SEIZE while exhaustive request source has no SEIZE arm | Ptrace is live; SEIZE is absent |
| L-C3 | 1 | Reverie query pairs were not reproducible | Closed by 14 exact focused/naive pairs and verified manifest |
| L-C4 / F-C2 | 1 | Preliminary schema profile omitted nonliteral forms | Closed by shared profile and reproduced 69/24/2/4 inventory; final trust gate remains separate |
| N-C3 | 1 | Ptrace-stub comments survive beside live implementation | Live dispatcher/source and exact-commit named fixtures prevail |
| N-C4 | 1 | MTE comments disagree with implementation and allocation coverage | Save/TCF source is current; broad tagging and hardware execution remain unresolved |
| N-C5 | 1 | Module-domain prose lags conditional live entry/exit | Conditional source implementation prevails; enforcement breadth is unexecuted |
| N-C6 | 1 | PCID is described as wired but skipped on available runners | Source-only; no demonstrated hardware enforcement |
| N-C7 | 1 | Broad compatibility prose conflicts with exact-commit failures and interface gaps | Use per-interface source and per-job execution evidence only |
| R-C1 | 1 | `BACKENDS.md` links older Reverie source | Discovery only; exact pinned implementation wins |
| R-C2 | 1 | Provider READMEs lag DBT/LiteInst/e9patch/SaBRe source | Exact current implementation wins; runtime remains unexecuted |
| N-C8 | 2 | Module docs disagree about real compiled-module execution | Loader source exists; real `.ko` execution remains unresolved |

No source disagreement is silently resolved as support. The ledger's
unresolved entries are product/runtime questions, not contradictory current
classifications.

## Unresolved decisions and missing evidence

- No external Hermit, strace, or gdb tracer ran against Narf; no
  Hermit-on-Narf backend runtime exists.
- Narf `PTRACE_SINGLESTEP`, exact syscall restart/wait ordering, realtime
  signal reinjection, FP/XSTATE, tracer-death cleanup, and SMP stopped-task
  synchronization are not independently demonstrated.
- Narf perf was not matched to Reverie's raw retired-conditional-branch timer
  contract; missing `GETSIGINFO` already prevents the stock timer path.
- The intended POC domain backend and actually executed protection are
  unspecified. PCID/MTE enforcement breadth is not runtime-proven.
- Exact native Linux versus Narf behavior for every process-vm page/segment
  fault fixture remains unresolved despite Narf's own behavior being known.
- Guest virtual-address equality versus a proven mapping bijection, and later
  PID/fd/inode/port renaming, must be fixed before execution.
- Cross-provider event membership and total ordering are not supplied by the
  generic Reverie traits; the proposed coverage receipt and ordering contract
  must be implemented and negatively tested.
- Exact boot-image staging of the same executable, interpreter, dependencies,
  filesystem, environment, and network fixture on Narf has not been executed.
- The cause and relevance of the pinned Narf x86_64 QEMU SIGSEGV and failed
  aarch64 kernel test remain unresolved.
- The current evidence is source- and captured-CI-bound. Public links were not
  interactively browser-tested; local exact revisions are the byte authority.

These unresolved items constrain the later recommendation and acceptance
tests. They do not contradict the locked current-state facts.

## Facts Audit B may not reinterpret

1. “Narf has no ptrace” is false, and “current Narf supports stock Reverie
   ptrace” is also false.
2. LiteInst's public/direct path and Hermit's selected ptrace-host hybrid are
   different paths.
3. E9patch is a public Reverie `Backend` implementation but, under Hermit's
   narrower local terminology, is preprocessing followed by ptrace—not a
   separate Hermit backend.
4. Missing lifecycle/seccomp events and messages, signal provenance,
   distinct-AS bulk memory, CPUID/RDTSC delivery, and cleanup cannot be hidden
   by basic syscall-stop fixtures.
5. Current Narf seccomp filter execution and user notification are source-only
   negatives. Only a proposed future notifier's behavioral contract is
   unresolved.
6. Narf's process-vm gather/scatter behavior is verified; exact paired Linux
   page/segment outcomes remain unresolved.
7. A BPF syscall program type does not prove syscall attachment, and hardware
   domains do not replace verifier/helper safety.
8. Return/errno equality without moved bytes, persistent effects, lifecycle,
   ordering, and complete coverage is not functional equivalence.
9. Record/replay fidelity is not independent Linux/Narf equivalence.
10. Proposed trace normalization, comparison classes, and A0–A18 are not
    implemented runtime support.
11. A declaration, test, comment, accepted option, callback registration, or
    zero exit is not an executed capability result.
12. Preliminary namespaced lane findings are not final trust-gate citations.

## Denominator check

| Figure | Denominator and admissible meaning |
|---|---|
| 2,956 Narf paths | All tracked paths at the pinned commit; not files read or capabilities |
| Query hit counts | Matching output lines; not unique files, capabilities, or support rates |
| 3 public Reverie `Backend` impls | All production `impl Backend for` blocks at `123df7c…`; excludes adapters/helpers and a documentation example |
| 6 execution-specific `Guest` impls | Concrete provider guests; excludes generic/test adapters |
| 11 provider/mechanism rows | Split direct/hybrid paths plus excluded `riptrace`; not eleven conforming Hermit backends |
| 61 lane records | 23 Narf + 18 Reverie + 20 Hermit; preliminary provenance, not final `F-N` findings |
| 99 lane `file_ref` records | 69 literal + 24 ellipsis + 2 whitespace-compacted + 4 unmarked omission/markup-normalized |
| 115 Reverie manifest entries | Runner/query/map inputs plus per-query receipts; integrity entries, not findings |
| 16 Narf domain IDs | Reserved identity namespace; not sixteen runtime-demonstrated enforcement domains |
| Provider test/ignore counts | Matching source files/annotations; not executed cases or pass rates |
| A0–A18 | Nineteen proposed acceptance primitives; zero executed cross-kernel cells |

No current-support percentage or provider success rate is admissible from
these figures.

## Runtime/source reconciliation

This auditor executed no product runtime test, QEMU guest, or Hermit command.
No ad-hoc Hermit invocation occurred, so `safehermit` was not needed.

The only product-execution evidence in the locked fact set is captured remote
CI at exact Narf commit `afe9199…`. Job `107842987657` visibly records fourteen
named ptrace ABI cases plus `smoke_process_ptrace_e2e` and
`smoke_process_ptrace_syscall_stop` as `[ OK ]`; it later records
`qemu-system-x86_64` exiting with SIGSEGV after 457 seconds and the job failing.
Other Narf job claims use the captured exact-SHA check-run snapshot.
Reverie/Hermit provider conclusions remain source-only.

## Goalpost-moving assessment

- Assertions weakened: **no**. No product assertion or test was edited.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. A15 still requires independent mutations of return, errno, buffer,
  schedule, signal, and output to be detected. A16 still requires refusal for
  missing or truncated coverage. A17/A18 retain the known memory and
  instruction prerequisites.
- Failure renamed or relabelled as a pass: **no**. The final seccomp correction
  promotes a known current negative from unresolved to absent/inert; it does
  not turn failure into support. The schema correction labels preliminary debt
  and keeps it ineligible for final verification.
- Check deleted instead of satisfied: **no**. Historical failed audits,
  contradiction ledgers, raw receipts, negative controls, and the separate
  final trust gate remain present.

The evidence bar was strengthened, not lowered.

## Validation performed

- Read the complete frozen contract, every current fact/coverage/matrix/finding
  file, and all four prior Audit A reports.
- Reconciled the registered slot and all source revisions and clean states.
- Parsed all 61 JSONL records; IDs are unique and dependencies resolve.
- Resolved and classified all 99 `file_ref` records against exact Git objects,
  reproducing the 69/24/2/4 distribution with zero missing source root, path,
  revision, or range.
- Reproduced the 115-entry Reverie manifest with `sha256sum -c`; its hash is
  `b41b2f2906679c83715ed4cb40d62d86296412d2601af61f0c3b1e885ee69bb5`.
- Reproduced all 22 Hermit query receipt status/line/byte/hash entries with zero
  mismatch; the manifest hash is
  `253b54fceece3db2bcb5dce97666c1c456bae29bc185105bccd131e1a1ba4d62`.
- Independently opened the load-bearing Narf ptrace request/option and syscall
  stop paths, seccomp handler and inverse consumers, process-vm transfer,
  TSC/CPUID handlers, BPF program/attach paths, domain confinement, scheduler,
  time, and loader evidence.
- Independently opened the Reverie contract and all three trait
  implementations, LiteInst direct/hybrid split, e9patch hybrid/direct split,
  stock ptrace option/filter/event/memory paths, KVM, DBT, preload, SaBRe, and
  user-notification helper evidence.
- Re-read the captured Narf x86_64 CI log and exact-head check-run snapshot.

## Reviewed input hashes

The lock is bound to these exact pre-existing inputs:

```text
ba01e11a74dcc798363a0e648b0fb25c764595bb750163dadc4baac645ba57e3  research-contract.md
ec741e902f4d9a25a4ed0563c20b8398006c50f03190002d2af8ba8558aecc77  evidence/README.md
eae5c53842e26ad6bf7a8df56456031ced62d1e880f790f7f879b208eb73de9b  evidence/audit-a.md
c0e89d56f1a6a69f0dc3d9a0105f0a87ffadcf2ab50c317bdff947ce9654eb56  evidence/audit-a-lock.md
059b73ff2eb7ca6b4d8ff092239a518481ce14b1ddf72fde7ee4e2a198cad156  evidence/audit-a-final.md
846d9eb268388833d03f71149e08325dc928a453871f4265ba69b84b9fffcfc0  evidence/audit-a-lock2.md
def65e554a23c81239c29b71d669b00b1c87389e6a6b533d9e7fdd01e29c5589  evidence/completion-checklist.md
816350c5eec7abaf0b9db2a714ba33d70c115e456ab8bed1bdb34253be4f212c  evidence/finding-schema-notes.md
2e46b263e971e5600a41bded386c98bdafc340c3e15caec401cab740c8df705b  evidence/hermit-facts.md
ff4c021c621c3631a7109ea92c5d98ef4c1ddc832f25ab29a757909a08d3ade2  evidence/hermit-findings.jsonl
4aca5282e782a676260204985a947ada8169e499a5d2f78218add710f0f53c5b  evidence/hermit-source-coverage.md
e060b30bfdab6edb0bba904e73656bdf8da22b656533ad28548760c30f523f87  evidence/narf-facts.md
2dc93dad4bfd789b5fc7e003d8b0fe7092dd48fd47edc77e30580775d8fc1c76  evidence/narf-findings.jsonl
87af8ecafaf2da777e80a7ee2a4c7560267b98ddb59648174efc31e358085cdc  evidence/narf-source-coverage.md
67ffe819a310930c492dbc026f5fd533f03663469afc056038a3bc3b9be1d6ef  evidence/reverie-backend-matrix.md
367b8066276332114e89fb7ccc3e9d54c9be28ac328726afad15b9766135cd76  evidence/reverie-facts.md
646e88c56843dde7d6087447d0009da456cb0e4cf15db245cf750f566ba425aa  evidence/reverie-findings.jsonl
418df560e828f6340c23fda2ff80d74738f8e0c6d25f1a4ef2eabc1da096663f  evidence/reverie-source-coverage.md
1cc00d4bd3108d482387976c23aaddc16398f75110cbed33f54357794226bb94  evidence/trace-input-matrix.md
```

Selected raw-evidence anchors:

```text
b41b2f2906679c83715ed4cb40d62d86296412d2601af61f0c3b1e885ee69bb5  reverie/query-pairs-v1/manifest.sha256
253b54fceece3db2bcb5dce97666c1c456bae29bc185105bccd131e1a1ba4d62  hermit/paired-queries-20260924/MANIFEST.txt
5b1291e92323a356ea245a98b2946f1ee79da5c6b8fe32c374ff82598820297c  narf/query-results.txt
0bcd75998b9a0b4d61c2aa5a7f06d0a3e823f56aa4189814e949dd3c17389d2d  narf/audit-a-correction-queries.txt
8e816cfdff7c65a33ea36005d7caf99694a2f23314ff8aaff040ca38d671f1d2d  narf/check-runs.json
0002f3dddb330d012e0aca4a3993d5d6185f67b2d82c393e718dd1f6c1d9ca64  narf/kernel-test-x86.log
```

Verdict: **LOCKED**. Audit B may begin.
