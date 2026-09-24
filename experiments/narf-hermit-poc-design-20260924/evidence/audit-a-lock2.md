# Audit A second lock review

## Verdict

**CHANGES REQUIRED.** Audit A is not locked and Audit B must not begin. The
final-audit snippet-profile defect is closed, and L-C1 through L-C4 otherwise
remain closed. One severity-0 contradiction remains in the canonical fact set:
`reverie-facts.md` still classifies current Narf “seccomp filter actions” as
**Unresolved**, while the pinned Narf implementation and the other corrected
ledgers establish that filter execution is inert and seccomp user notification
is absent at this revision. Only a proposed future notifier's behavioral
contract is unresolved.

This was a fresh, read-only correctness audit. The only file written by this
auditor is this report. No Hermit, Narf guest, QEMU guest, or product runtime
test was run.

## Review target and source identity

- Parent `dev-hermit` start:
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`.
- Registered worktree branch: `codex/narf-hermit-design`, still at that start
  revision during this review.
- [`rrnewton/hermit@ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2).
- Hermit's build-time
  [`rrnewton/reverie@ae1d1da78d2d89a1fa0454ee2282cf135e2286ed`](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed).
- Separately inventoried
  [`rrnewton/reverie@123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937).
- [`hodgesds/narf@afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/tree/afe919924a5fd2c8e820ab3a0564b6d45438e6bc).
- Linux man-pages:
  `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`.

The registered worktree's Hermit, nested Reverie, and standalone Reverie
checkouts matched the three stated product revisions. The ignored Narf and
Linux man-pages clones matched their stated revisions and were clean. Hermit's
actual build pin remains distinct from the standalone Reverie inventory pin.

## Findings requiring correction

### Severity 0 — F-C1 is still open in `reverie-facts.md`

[`reverie-facts.md:155`](reverie-facts.md) says:

> **Unresolved:** current Narf semantics for ... seccomp filter actions ...
> remain outside the established fact set.

That sentence is a current-capability claim, not a future-design question. It
contradicts the primary source and the corrected statements in
[`narf-facts.md`](narf-facts.md), [`hermit-facts.md`](hermit-facts.md), and
[`reverie-source-coverage.md`](reverie-source-coverage.md):

- pinned Narf's complete `sys_seccomp` handler ignores the filter pointer and
  stores mode `2` for both strict and filter operations;
- its live syscall dispatcher has no seccomp-filter evaluation path;
- `SECCOMP_FILTER_FLAG_NEW_LISTENER` returns `EINVAL`;
- repository-wide precise and inverse searches find no notification object,
  receive/send/addfd ioctl lifecycle, or blocked-task notifier state;
- recognizing action constants in `SECCOMP_GET_ACTION_AVAIL` does not execute
  those actions.

The correct disposition is:

- **Verified current (source-only):** seccomp filter execution, including the
  `RET_TRACE` mechanism stock Reverie uses on Linux, is absent/inert on current
  Narf;
- **Verified current (source-only):** seccomp user notification is absent on
  current Narf because listener creation is rejected and the notification
  lifecycle does not exist;
- **Unresolved:** restart, cancellation, memory movement, listener lifetime,
  and ioctl semantics for a *proposed future* Narf notifier.

Impact: leaving “seccomp filter actions” in the current unresolved list permits
Audit B to describe a known negative as merely unknown. That weakens the
interposition-option comparison and obscures an independent fatal gap for stock
`PtraceBackend`. Remove that phrase from the current unresolved list or replace
the row with the three-way disposition above. Do not promote source evidence to
an executed runtime result, and do not claim the feature can never be added.

No other severity-0 or severity-1 issue was found in this review.

## Requested closure checks

### F-C2 — complete lane snippet-profile enumeration: closed

The three finding files are byte-identical to the set measured by the previous
auditor. A fresh exact-revision comparison of all `file_ref.snippet` values to
their cited line ranges reproduced the complete 99-reference denominator:

- 69 literal contiguous snippets;
- 24 snippets containing an explicit ellipsis;
- 2 whitespace-compacted snippets: `N-F-3` and `N-F-5`;
- 4 unmarked omission or Markdown-normalized snippets: `N-F-1`, `N-F-12`,
  `N-F-13`, and `N-F-20`;
- 0 unresolved source roots, revisions, paths, or line ranges.

[`finding-schema-notes.md`](finding-schema-notes.md) now names the shared
`narf-hermit-research-lane/v1` profile, applies it to all three lane files,
enumerates all three nonliteral forms, names the four third-form records, and
forbids canonical verification of every nonliteral lane receipt. It also
requires a separate `deep-research-finding-schema/v1` `findings.jsonl` with
unique `F-N` IDs, literal snippets, and a separate verifier output before final
publication. This closes F-C2 without weakening the final trust gate.

### L-C1 through L-C4

| Closure item | Result | Reproduced basis |
|---|---|---|
| L-C1 — LiteInst path inversion | **Closed** | `impl Backend for LiteinstBackend` calls direct/in-guest `run_with_preload*`; `run_host_with_preload*` is separate and Hermit explicitly selects it. R-Q9, facts, matrix, and `R-F14` agree. |
| L-C2 — process-vm fact versus paired Linux outcome | **Closed** | Narf's self/same-AS-only check, distinct-AS `EPERM`, gather-before-scatter, and `EFAULT` paths are source-verified. Stock external Reverie bulk memory is incompatible. Exact paired Linux page/segment outcomes remain explicitly **Unresolved**. |
| L-C3 — Reverie precise/naive receipts | **Closed** | R-Q1–R-Q14 each name one focused and one exact two-word naive query with command/stdout/stderr/exit receipts. The 115-entry manifest verified with `sha256sum -c`; no receipt is reused. |
| L-C4 — shared preliminary schema profile | **Closed** | The versioned shared profile and complete nonliteral-form inventory now exist, and preliminary receipts remain ineligible for final verification. |

### Earlier severity-0 findings

The three original severity-0 findings remain closed:

1. LiteInst's public Reverie `Backend` path is direct/in-guest; Hermit's
   ptrace-owned hybrid is a separate API.
2. Current Narf rejects external-tracer distinct-address-space
   `process_vm_readv/writev`, which is fatal for stock Reverie bulk memory.
3. Default fail-closed Detcore subscribes to CPUID and RDTSC; Narf accepts but
   does not enforce `PR_SET_TSC(PR_TSC_SIGSEGV)` and lacks
   `ARCH_SET_CPUID`, so those Tool events cannot be delivered by current Narf.

## Canonical fact set independently spot-checked

Subject to correction of the one contradictory sentence above, these facts are
supported by the pinned primary sources:

1. **Verified current:** Reverie's backend-neutral contract is
   `Backend`/`Tool`/`Guest`/`GlobalTool`/`Subscription`/`MemoryAccess`, not only
   a syscall callback. A conforming backend owes arbitrary-Tool hosting,
   process-tree lifecycle, subscribed events, mutation/data movement, teardown,
   output, and nonfabricated statistics.
2. **Verified current:** exactly three production Rust blocks implement the
   public Reverie `Backend` trait at `123df7c…`: ptrace, LiteInst, and e9patch.
   KVM, DBT, preload, SaBRe/host, and user notification are real narrower
   providers/helpers, not additional public trait implementations.
3. **Verified current:** stock ptrace uses TRACEME plus an initial stop and one
   option mask containing EXEC, EXITKILL, CLONE, FORK, VFORK, VFORKDONE, EXIT,
   SECCOMP, and TRACESYSGOOD. Selective seccomp `RET_TRACE` supplies ordinary
   subscribed-syscall entry events.
4. **Verified current:** stock ptrace requires event/status decoding,
   `GETEVENTMSG`, `GETSIGINFO`, GPR/FP/XSTATE regsets, PEEK/POKE,
   distinct-address-space process-vm bulk transfer, SINGLESTEP/SYSCALL resume,
   and tracer-death containment across its full state machine.
5. **Verified current:** Narf has real ptrace. Its pinned x86_64 QEMU log shows
   named basic ptrace fixtures and two process ptrace smokes passing. This is
   narrower than external-tracer compatibility, and the same job failed later
   when QEMU exited with SIGSEGV after 457 seconds.
6. **Verified current:** current Narf is not compatible with stock Reverie
   ptrace. Lifecycle and seccomp events/messages are absent, `GETSIGINFO` is
   absent, most accepted option bits are inert, stopped-state/signal semantics
   are incomplete, external bulk memory is rejected, and default CPUID/RDTSC
   interception is absent or inert.
7. **Verified current:** Narf's live syscall dispatcher contains concrete
   ptrace entry/exit calls and a textual tracing block, but no generic typed
   syscall-interposition trait.
8. **Verified current:** Narf has verifier, maps, fd lifetimes, atomic and
   sleepable BPF execution, and domain guards. `BPF_PROG_TYPE_SYSCALL` can be
   loaded and test-run, but the attach resolver exposes fentry and XDP only;
   there is no guest-syscall attach target or trace-context ABI.
9. **Verified current:** hardware domains are conditional defense in depth.
   FRAME/domain 0 remains writable while BPF runs, so verifier, interpreter/JIT,
   helpers, dispatcher, and domain manager correctness remain trusted.
10. **Verified current:** strict Hermit comparison preserves substantive values
    and requires complete structured records and I/O-buffer evidence. Current
    record/replay proves fidelity to one recording, not equality of independent
    Linux and Narf executions.
11. **Proposed:** every trace field, normalization, failure class, coverage
    receipt, and A0–A18 case in `trace-input-matrix.md` is a candidate
    cross-kernel contract. None is current Hermit-on-Narf runtime support.
12. **Above the API:** identical executable/dependency/filesystem/environment
    and controlled network inputs are harness obligations.
13. **Deferred:** external networking, performance equivalence, arbitrary
    guest-identity/address renaming, and production hardening are outside the
    first functional POC.

## Current provider and Narf-option matrices

The provider inventory is complete for the pinned source and keeps source
existence separate from runtime execution:

| Provider/mechanism | Public Reverie `Backend` | Current disposition |
|---|---:|---|
| ptrace | Yes | External Linux ptrace/seccomp reference backend; source-only in this research |
| KVM | No | Bounded x86_64 bare Linux-like personality; **Optional capability / Software helper** |
| DynamoRIO DBT | No | In-process translated provider; **Optional capability / Software helper** |
| preload | No | Trusted in-process seccomp-TRAP primitive; **Software helper** |
| LiteInst trait/direct | Yes | Bounded direct in-guest backend |
| LiteInst ptrace-host hybrid | No separate impl | Separate optional path selected by Hermit |
| e9patch hybrid | Yes | Root rewrite plus ptrace lifecycle/residual coverage |
| e9patch direct | No separate impl | Optional in-process path with incomplete lifecycle |
| SaBRe + reverie-host | No | Experimental adapter/helper |
| seccomp user notification | No | Reusable Linux notifier helper, no execution backend consumer |
| `riptrace` | No | Frontend; excluded as a provider/backend |

The current-Narf option comparison also remains complete:

| Option | Current-Narf disposition |
|---|---|
| Stock ptrace | **Not viable unchanged:** several independently fatal event, memory, instruction, and lifecycle gaps |
| Seccomp user notification | **Verified current source-only absent/inert:** filter execution and listener lifecycle do not exist |
| Ptrace-compatible emulation | **Proposed:** large Linux-compatibility project including events, memory, regsets, signals, instruction traps, and cleanup |
| First-class in-kernel Rust interposition | **Proposed:** nearest insertion seam is the live dispatcher; lifecycle, ownership, order, memory, authorization, reentrancy, and teardown remain design obligations |
| Framekernel/BPF-like extension | **Proposed on existing ingredients:** requires syscall attach points and a typed context/copy ABI; verifier is primary isolation and hardware domains are defense in depth |

## Contradiction ledger

| ID | Severity | Conflict | Canonical disposition |
|---|---:|---|---|
| A2-C1 | 0 | `reverie-facts.md:155` calls current Narf seccomp filter actions unresolved | Current filter execution and notification are absent/inert by pinned source; only a future notifier contract is unresolved |
| N-C1 | 0 | Narf docs describe a `linux-compat` feature absent from Cargo/source | Linux ABI is always present at this pin; prose is stale |
| N-C2 | 0 | Narf docs claim SEIZE while exhaustive request source has no arm | Ptrace is live; SEIZE is absent |
| N-C3 | 1 | Ptrace-stub comments survive beside a live implementation | Live dispatcher/source and executed named fixtures prevail |
| N-C4 | 1 | MTE comments disagree with implementation and allocation coverage | Save/TCF source is current; broad tagging and hardware execution remain unresolved |
| N-C5 | 1 | Module-domain prose lags conditional live entry/exit | Conditional source implementation prevails; enforcement breadth is unexecuted |
| N-C6 | 1 | PCID is described as wired but skipped on available runners | Source-only; no demonstrated hardware enforcement |
| N-C7 | 1 | Broad compatibility prose conflicts with exact-commit job failures and interface gaps | Use per-interface source and per-job execution evidence only |
| N-C8 | 2 | Module docs disagree about real compiled-module execution | Loader source exists; real `.ko` execution remains unresolved |
| R-C1 | 1 | `BACKENDS.md` links older source | Discovery only; exact pinned implementation wins |
| R-C2 | 1 | Provider READMEs lag current DBT/LiteInst/e9patch/SaBRe source | Exact current implementation wins; runtime remains unexecuted |

Closed historical contradictions—LiteInst path inversion, current Narf
process-vm status, default CPUID/RDTSC requirements, query receipts, and the
preliminary schema inventory—remain visible in the prior audits and were not
silently deleted.

## Facts Audit B may not reinterpret

If the remaining contradiction is corrected and a fresh lock succeeds, Audit B
must treat these as immutable:

1. “Narf has no ptrace” is false, and “current Narf supports stock Reverie
   ptrace” is also false.
2. LiteInst's public/direct path and Hermit's selected ptrace-host hybrid are
   different paths.
3. E9patch is a public Reverie `Backend` implementation but, under Hermit's
   narrower local terminology, is preprocessing followed by ptrace—not a
   separate Hermit backend.
4. Missing lifecycle/seccomp events, event messages, signal provenance,
   external bulk memory, CPUID/RDTSC delivery, and cleanup cannot be hidden by
   basic syscall-stop fixtures.
5. Narf seccomp filter execution and user notification are current source-only
   negatives. Future notifier restart/cancellation/memory/lifecycle semantics
   are unresolved.
6. Narf's own process-vm gather/scatter behavior is verified; exact paired
   Linux page/segment results remain unresolved.
7. A BPF syscall program type does not prove syscall attachment, and hardware
   domains do not replace verifier/helper safety.
8. Return/errno equality without moved bytes, persistent effects, lifecycle,
   ordering, and complete coverage is not functional equivalence.
9. Record/replay fidelity is not independent Linux/Narf equivalence.
10. Proposed trace normalization and A0–A18 are not implemented runtime support.
11. A declaration, test, comment, accepted option, callback registration, or
    zero exit is not an executed capability result.

## Denominator check

| Figure | Denominator and admissible meaning |
|---|---|
| 2,956 Narf paths | All tracked paths at the pinned commit; not files read or capabilities |
| Query hit counts | Matching output lines; not unique files, capabilities, or support rates |
| 3 public Reverie `Backend` impls | All production `impl Backend for` blocks at `123df7c…`; excludes adapters/helpers |
| 6 execution-specific `Guest` impls | Concrete provider guests after excluding generic/test adapters |
| 11 provider/mechanism rows | Includes split direct/hybrid paths and excluded `riptrace`; not eleven conforming Hermit backends |
| 61 lane records | 23 Narf + 18 Reverie + 20 Hermit; preliminary provenance, not final `F-N` findings |
| 99 lane `file_ref` records | 69 literal + 24 ellipsis + 2 whitespace-compacted + 4 unmarked omission/markup-normalized |
| 115 Reverie manifest entries | Runner/query/map inputs plus per-query receipts; integrity entries, not findings |
| 16 Narf domain IDs | Reserved identity namespace; not sixteen runtime-demonstrated enforcement domains |
| Provider test/ignore counts | Matching source files/annotations; not executed cases or pass rates |
| A0–A18 | Nineteen proposed acceptance primitives; zero executed cross-kernel cells |

No current-support percentage or provider success rate is admissible from these
figures.

## Runtime/source reconciliation

This auditor executed no product runtime test, QEMU guest, or Hermit command.
No ad-hoc Hermit invocation occurred, so `safehermit` was not needed.

The only product execution evidence in the fact set is captured remote CI at
the exact Narf commit. The x86_64 job log visibly records the named ptrace ABI
fixtures plus `smoke_process_ptrace_e2e` and
`smoke_process_ptrace_syscall_stop` as `[ OK ]`; it later records QEMU exiting
with SIGSEGV after 457 seconds and the job failing. No external Hermit, strace,
or gdb tracer ran against Narf. Reverie/Hermit provider conclusions remain
source-only.

## Goalpost-moving assessment

- Assertions weakened: **no**. No product assertion or test was edited.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. A15/A16 retain mutation sensitivity and fail-closed
  coverage/truncation controls; A17/A18 retain known memory and instruction
  prerequisites.
- Failure renamed or relabelled as a pass: **yes, one remaining evidence
  classification.** `reverie-facts.md:155` labels a known current seccomp
  absence as unresolved. This is less severe than calling it supported, but it
  still lowers the evidence bar and blocks the lock.
- Check deleted instead of satisfied: **no**. Prior failed audits, raw receipts,
  contradiction ledgers, negative controls, and the separate final trust gate
  remain present.

The snippet-profile correction did not weaken the final schema: it made the
preliminary deviation inventory explicit while keeping every nonliteral lane
receipt ineligible for canonical verification.

## Validation performed

- Read the frozen contract, every current fact/coverage/matrix/finding file,
  and all three prior Audit A reports.
- Reconciled exact source revisions in the registered worktree and ignored
  clones.
- Parsed all 61 JSONL records; IDs are unique and all dependencies resolve.
- Resolved all 99 `file_ref` records to exact revisions/ranges and reproduced
  the 69/24/2/4 snippet classification.
- Reproduced the 115-entry Reverie query-receipt manifest with
  `sha256sum -c`.
- Independently opened the load-bearing Narf seccomp handler/dispatcher,
  ptrace request/option source, process-vm transfer, TSC/CPUID handlers, BPF
  program/attach surfaces, and domain-confinement source.
- Independently opened the Reverie `Backend` implementations, LiteInst direct
  and hybrid paths, Hermit dispatch, stock ptrace option/filter setup, and
  external memory transport.
- Re-read the captured exact-head Narf x86_64 job log and check-run snapshot.

## Reviewed input hashes

The verdict is bound to these current pre-existing evidence inputs:

```text
ba01e11a74dcc798363a0e648b0fb25c764595bb750163dadc4baac645ba57e3  research-contract.md
ec741e902f4d9a25a4ed0563c20b8398006c50f03190002d2af8ba8558aecc77  evidence/README.md
059b73ff2eb7ca6b4d8ff092239a518481ce14b1ddf72fde7ee4e2a198cad156  evidence/audit-a-final.md
c0e89d56f1a6a69f0dc3d9a0105f0a87ffadcf2ab50c317bdff947ce9654eb56  evidence/audit-a-lock.md
eae5c53842e26ad6bf7a8df56456031ced62d1e880f790f7f879b208eb73de9b  evidence/audit-a.md
def65e554a23c81239c29b71d669b00b1c87389e6a6b533d9e7fdd01e29c5589  evidence/completion-checklist.md
816350c5eec7abaf0b9db2a714ba33d70c115e456ab8bed1bdb34253be4f212c  evidence/finding-schema-notes.md
2e46b263e971e5600a41bded386c98bdafc340c3e15caec401cab740c8df705b  evidence/hermit-facts.md
ff4c021c621c3631a7109ea92c5d98ef4c1ddc832f25ab29a757909a08d3ade2  evidence/hermit-findings.jsonl
4aca5282e782a676260204985a947ada8169e499a5d2f78218add710f0f53c5b  evidence/hermit-source-coverage.md
e060b30bfdab6edb0bba904e73656bdf8da22b656533ad28548760c30f523f87  evidence/narf-facts.md
2dc93dad4bfd789b5fc7e003d8b0fe7092dd48fd47edc77e30580775d8fc1c76  evidence/narf-findings.jsonl
87af8ecafaf2da777e80a7ee2a4c7560267b98ddb59648174efc31e358085cdc  evidence/narf-source-coverage.md
67ffe819a310930c492dbc026f5fd533f03663469afc056038a3bc3b9be1d6ef  evidence/reverie-backend-matrix.md
f718177e2d5468ec153e7673a22afbd666b56a40b9a47859bfbe788a7533f3a1  evidence/reverie-facts.md
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

**Final verdict: CHANGES REQUIRED. Audit B may not begin.**
