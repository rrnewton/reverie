# Audit A rerun — correctness lock attempt

## Verdict

**CHANGES REQUIRED.** Audit A is not locked and Audit B must not begin. The
three severity-0 source facts that triggered the first audit were reproduced,
and the main fact sets now state them correctly. However, the coverage ledger
still contains the old LiteInst inversion, the process-vm partial-transfer
status disagrees across current fact sets, and the query/schema controls are
not yet strong enough to lock the evidence tree.

This was a fresh read-only correctness audit. The only file written by the
auditor is this report.

## Review boundary

The reviewed source revisions are:

- parent `dev-hermit`: `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`;
- [`rrnewton/hermit@ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2);
- Hermit's build-time [`rrnewton/reverie@ae1d1da78d2d89a1fa0454ee2282cf135e2286ed`](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed);
- separately inventoried [`rrnewton/reverie@123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937);
- [`hodgesds/narf@afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/tree/afe919924a5fd2c8e820ab3a0564b6d45438e6bc);
- Linux man-pages `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`.

All five local source trees were clean and their `HEAD` values matched those
revisions. `ae1d1da…` is seven commits after `123df7c…`; the critical
`Backend`/`Tool`/`Guest`/`Subscription`, task, safeptrace, and memory files
checked by this audit are byte-identical across the pins. Hermit runtime facts
continue to use its actual build pin.

## Findings requiring correction

### Severity 0 — the LiteInst correction is still contradicted by the coverage ledger

[`reverie-source-coverage.md:27`](reverie-source-coverage.md) still concludes
that LiteInst is a “Conforming hybrid backend plus narrower direct option.”
That is the exact classification the first Audit A rejected.

The primary source is unambiguous:

- [`LiteinstBackend::run_with_preload`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L361-L405)
  is the direct in-guest/UDS path and explicitly does not attach ptrace;
- [`impl Backend for LiteinstBackend`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L590-L645)
  delegates all three trait methods to that direct family;
- [`run_host_with_preload*`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L204-L359)
  is a separate ptrace-owned hybrid API;
- pinned Hermit explicitly selects the separate hybrid in
  [`hermit-cli/src/lib.rs`](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L2756-L2771).

`reverie-facts.md`, `reverie-backend-matrix.md`, `R-F14`, and the Hermit facts
are corrected. The source-coverage conclusion is not. This is load-bearing:
“conforming hybrid” lends ptrace lifecycle properties to the public trait path
that does not own them.

Required correction: replace the R-Q9 result with the same split used by the
matrix: public trait/direct path with bounded direct lifecycle, plus a separate
optional ptrace-host hybrid explicitly selected by Hermit. Search the complete
evidence tree for any other unqualified “LiteInst hybrid backend” statement and
keep e9patch's distinct hybrid classification unchanged.

### Severity 1 — process-vm partial-transfer status is internally contradictory

The verified load-bearing result is consistent and correct: current Narf
accepts only self or same-`AddressSpace` `process_vm_readv/writev`, returns
`EPERM` for a distinct target address space, and stock Reverie invokes those
syscalls from a distinct external tracer. This is fatal for stock Reverie's
bulk `MemoryAccess` path.

The secondary partial-transfer claim is not reconciled:

- `narf-facts.md:111,292` says page/segment partial-count compatibility is
  known wrong and that gather/scatter behavior “diverges” from Linux;
- `hermit-facts.md:155-158`, `reverie-facts.md:72,98,155`,
  `reverie-backend-matrix.md:76`, and `reverie-source-coverage.md:115` preserve
  it as **Unresolved**;
- the pinned Linux man page says a fault *may* yield a partial transfer and
  explains page-granularity behavior, but no executed Linux/Narf pair or pinned
  Linux implementation path establishes the exact outcome for each proposed
  boundary case.

Narf source does verify its own current behavior: it gathers whole selected
segments, returns `EFAULT` on a gather fault, may have performed earlier
destination writes before a scatter fault, and still returns `EFAULT` rather
than a byte count. That source fact is distinct from proving the exact native
Linux result for every A2 boundary fixture.

Required correction: keep distinct-AS `EPERM` and its stock-Reverie impact
**Verified current (source-only)**. State Narf's gather/scatter behavior as a
verified source fact, but preserve the cross-kernel page/segment comparison as
**Unresolved** until a pinned Linux implementation analysis or an executed
paired boundary test resolves it. Remove the unsupported blanket “wrong” or
“diverges” conclusion, or add the missing primary evidence.

### Severity 1 — Reverie paired-query receipts are incomplete

The Narf ledger records thirteen precise/naive pairs and the Hermit lane now
has an integrity-checked manifest for eleven pairs. The Reverie ledger lists
fourteen research questions, but it records query *classes*, not the exact
commands/regular expressions, and its raw directory has no explicit naive
partner for at least the execution-specific `Guest` inventory, Tool callback
surface, test/runtime inventory, and unlisted-provider inverse.

An inverse query is not a naive control. The current files cannot reproduce the
claimed pair for every R-Q row from the ledger alone.

Required correction: for every R-Q1–R-Q14 row, record the exact focused query,
the exact two-or-three-word naive query, stdout/stderr, exit status, and a
manifest hash. A zero-hit naive query is valid and must be preserved. If a
single raw result is intentionally reused by multiple questions, name that
mapping explicitly rather than implying a missing file exists.

### Severity 1 — the lane schema boundary is clear, but is not fully versioned or described

`finding-schema-notes.md` correctly says the three lane JSONL files are not
final trust-gate input, requires a new canonical `findings.jsonl` with `F-N`
IDs and literal snippets, and requires a separate `verification.jsonl`. This
is a sound fail-closed publication boundary and must remain.

The claimed “versioned deviation” has no single explicit profile identifier
covering all three lanes. The current corpus contains 61 unique lane IDs and
99 `file_ref` records. Twenty-four snippets contain human-readable ellipses.
Of the other 75 snippets, 73 are literal within their cited ranges and two are
whitespace-compacted paraphrases (`N-F-3` and `N-F-20`), a deviation the note
does not mention. Only the Hermit ledger names a profile (`hermit-evidence/v1`).

Required correction: name one repository-local profile and version, for
example `narf-hermit-research-lane/v1`, state that it governs all three lane
files, and enumerate both nonliteral forms: ellipsis-bearing and
whitespace-compacted snippets. State mechanically that none may receive a
canonical exact-verification verdict and that the final trust-gate input is a
different file/profile. Do not relabel the preliminary lane records as
verified.

## Corrections independently reproduced

The following severity-0 facts from the first audit now reproduce in primary
source. They are accurate but the overall fact set remains unlocked because of
the findings above.

1. **Verified current:** LiteInst's public Reverie `Backend` implementation is
   direct/in-guest; the ptrace-host hybrid is a separate API selected by Hermit.
2. **Verified current (source-only):** Narf `process_vm_*` is self/same-AS only
   and returns `EPERM` for a distinct address space; this prevents stock
   external Reverie bulk memory independently of the other ptrace gaps.
3. **Verified current (source-only):** fail-closed Detcore returns
   `Subscription::all()`, which includes CPUID and RDTSC. Reverie requests
   `PR_SET_TSC(PR_TSC_SIGSEGV)` and injects `ARCH_SET_CPUID(0)`. Narf accepts
   the TSC request without disabling RDTSC and its FS/GS-only `arch_prctl`
   rejects the CPUID operation. Current Narf therefore cannot deliver either
   default instruction event.

## Coverage and inventory audit

The corrected heading inventory passes this rerun: every opened Markdown
document is either assigned a complete heading list, marked heading-only, or
explicitly classified discovered-not-opened. The previously omitted
`STATUS.md`, `epoll_hang.md`, research summaries, and provider README controls
are now accounted for. Document-class tables, seen-and-excluded ledgers, and
inverse checks are present for all three lanes.

The surface inventory is otherwise complete enough for another lock attempt:

- the public Reverie trait search yields exactly three implementations:
  ptrace, LiteInst, and e9patch; the generic `IntoGuest` adapter is not counted;
- the six execution-specific `Guest` implementations are separated from the
  public `Backend` count;
- the provider matrix covers ptrace, KVM, DBT, preload, both LiteInst paths,
  both e9patch paths, SaBRe/host, seccomp user notification, and excluded
  `riptrace`;
- the current-Narf interposition matrix covers stock ptrace, user notification,
  ptrace-compatible emulation, a first-class Rust kernel interface, and a
  framekernel/BPF-like extension;
- Narf x86_64/aarch64 boot, ptrace, domain-backend, loader, scheduling, time,
  BPF, module, and security differences are inventoried;
- all current and proposed behavior is separated, with no source declaration,
  test name, package name, accepted option, or callback registration promoted
  to an executed pass.

The terminology control for Audit B must be explicit: “Reverie public
`Backend` implementation” is not interchangeable with Hermit's narrower local
definition of a complete Detcore execution backend. In particular, pinned
Reverie has `impl Backend for E9patchBackend`, while `hermit/AGENTS.md` requires
e9patch to be described as preprocessing with ptrace in Hermit terminology.

## Canonical facts that remain valid but are not locked

Subject to the corrections above, the following controls survived independent
source spot-checks across every major provider/platform:

- **Verified current:** Narf ptrace is live and has executed exact-commit x86_64
  QEMU fixtures, but stock Reverie remains incompatible because its seccomp and
  lifecycle events/messages, signal provenance, cross-AS bulk memory,
  CPUID/RDTSC interception, regset breadth, and cleanup semantics are missing or
  divergent.
- **Verified current:** current Narf stores seccomp mode/action-shaped state but
  does not evaluate filters, rejects `NEW_LISTENER`, and has no notification
  object/ioctl lifecycle.
- **Verified current:** Narf has no generic typed syscall-interposition trait;
  concrete ptrace entry/exit calls and textual logging are the live seams.
- **Verified current:** Narf's BPF runtime has verified program/map/lifetime and
  atomic/sleepable machinery, but a syscall program type is test-run only and
  has no syscall attach/context ABI.
- **Verified current:** Narf hardware domains are conditional defense in depth;
  FRAME/domain 0 remains accessible and the verifier/typed helpers remain the
  primary extension boundary.
- **Verified current:** KVM, DBT, preload, SaBRe, and user notification are
  real provider/helper paths but not public Reverie `Backend` implementations.
  e9patch's public trait path is ptrace-hosted; its direct path is separate.
- **Verified current:** strict Hermit comparison removes only wall-clock log
  prefix and canonicalizes marked host addresses; record/replay verifies one
  recording's replay, not independent Linux/Narf equivalence.
- **Proposed:** all trace fields, normalizations, failure classes, and A0–A18
  acceptance cases are candidate cross-kernel contracts, not implemented
  Hermit-on-Narf runtime support.
- **Above the API:** executable, dependency, filesystem, environment, and
  network fixture equality belongs to the harness.
- **Deferred:** external networking, performance equivalence, arbitrary guest
  identity/address renaming, and production hardening are outside the first
  functional POC.

These statements are not an authorization to start Audit B. A corrected exact
evidence head requires another read-only lock check.

## Contradiction and immutable-fact controls

The existing Narf and Reverie contradiction ledgers preserve the stale
`linux-compat` feature prose, false SEIZE documentation, ptrace-stub comments,
MTE/domain/module documentation drift, old provider READMEs, accepted-but-inert
ptrace options, and source/runtime limits. No earlier severity-0 contradiction
was silently removed.

The next lock attempt must add and resolve these current entries:

| ID | Severity | Conflict | Required disposition |
|---|---:|---|---|
| L-C1 | 0 | Reverie R-Q9 still says LiteInst public path is hybrid | Direct public trait path; separate ptrace-host hybrid |
| L-C2 | 1 | Narf calls partial transfer a proved Linux divergence while other lanes keep it unresolved | Separate verified Narf behavior from unresolved paired Linux outcome |
| L-C3 | 1 | Reverie says every question has a precise/naive pair but raw/query text does not reproduce all pairs | Record exact pair receipts for R-Q1–R-Q14 |
| L-C4 | 1 | Lane schema note claims a versioned deviation but has no shared profile/version and omits two nonliteral snippets | Name/version the profile and enumerate every deviation |

Until those rows are closed, Audit B may not reinterpret any of these controls:

1. Narf has real ptrace, but does not support stock Reverie ptrace.
2. LiteInst direct/public and Hermit-selected ptrace-host paths are distinct.
3. Distinct-AS process-vm, lifecycle event, signal provenance, CPUID/RDTSC, and
   cleanup gaps cannot be hidden by basic syscall-stop fixtures.
4. Seccomp mode names do not prove filter or notifier execution.
5. BPF syscall program type does not prove syscall attachment.
6. Hardware domains do not replace verifier/helper safety.
7. Return/errno equality without moved bytes and persistent effects is not
   functional equivalence.
8. Record/replay fidelity is not independent cross-kernel equivalence.
9. Proposed trace/normalization/tests are not runtime support.
10. A declaration, test, comment, accepted option, or zero exit is not an
    executed capability result.

## Denominator check

| Figure | Denominator and disposition |
|---|---|
| 2,956 Narf paths | All tracked paths at the pinned commit; not files read and not capabilities |
| Query hit counts | Matching output lines; not unique files/capabilities and not support metrics |
| 3 public `Backend` impls | All actual Rust `impl Backend for` blocks at `123df7c…`; excludes generic adapter and helpers |
| 6 execution-specific `Guest` impls | Concrete provider guests; excludes generic `IntoGuest` |
| 11 provider/mechanism rows | Includes split direct/hybrid rows and excluded `riptrace`; not eleven conforming backends |
| 61 lane JSON records | 23 Narf + 18 Reverie + 20 Hermit; all unique IDs; zero canonical `F-N` IDs |
| 99 JSON `file_ref` records | All ranges exist; 24 ellipsis-bearing, 73 exact-literal, 2 whitespace-compacted |
| 16 domain IDs | Reserved namespace, not sixteen runtime-demonstrated enforcement domains |
| Provider test/ignore counts | Matching source files and annotation occurrences, not executed cases or pass rates |
| Narf CI pass statements | Named visible job conclusions or `[ OK ]` lines; the x86_64 kernel-test job still failed after 457 seconds |
| A0–A18 | Nineteen proposed acceptance IDs; zero executed cross-kernel cells |

No current-support percentage or provider success rate is admissible from these
figures.

## Runtime/source reconciliation

This auditor executed **no runtime test**, no QEMU guest, and no Hermit command.
No ad-hoc Hermit invocation occurred, so `safehermit` was not needed.

The only executed product evidence in the research set is captured remote CI
from Narf's exact commit. GitHub job `107842987657` visibly reports the named
x86_64 ptrace fixtures and two process ptrace smokes as `[ OK ]`, then reports
`qemu-system-x86_64` exiting with SIGSEGV after 457 seconds and the job
conclusion is failure. Other cited job conclusions are exact-SHA captured API
records. Reverie/Hermit provider conclusions remain source-only.

## Goalpost-moving assessment

- Assertions weakened: **no**. No product/test assertion was edited by this
  evidence-only change.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. A17/A18 strengthen the acceptance set; A15/A16 retain negative and
  refusal controls.
- Failure renamed or relabelled as a pass: **yes, one stale classification**.
  R-Q9 still calls the bounded direct LiteInst trait path a conforming hybrid,
  which grants it the wrong lifecycle evidence. This is L-C1 and blocks lock.
- Check deleted instead of satisfied: **no**. The completion checklist remains
  open and this audit refuses the lock.

There is no evidence of a tolerance or comparator being deliberately lowered;
the remaining classification error nevertheless has the same effect and must
be corrected rather than exempted.

## Reviewed input hashes

The verdict is bound to these exact inputs:

```text
c2b88604c7acf57b4d10408370f5bef2fbabd3fb9fd1421a2e8fdf54ea93705d  research-contract.md
ec741e902f4d9a25a4ed0563c20b8398006c50f03190002d2af8ba8558aecc77  evidence/README.md
eae5c53842e26ad6bf7a8df56456031ced62d1e880f790f7f879b208eb73de9b  evidence/audit-a.md
def65e554a23c81239c29b71d669b00b1c87389e6a6b533d9e7fdd01e29c5589  evidence/completion-checklist.md
ce1292097668cc15ab35a8a31c621a0b57080bba0b3ddad90fcfc81490fd8b28  evidence/finding-schema-notes.md
105568d678dbf5f5ebfe0acaa5ee4761765519716598f13de384f708551dbac5  evidence/hermit-facts.md
ff4c021c621c3631a7109ea92c5d98ef4c1ddc832f25ab29a757909a08d3ade2  evidence/hermit-findings.jsonl
4aca5282e782a676260204985a947ada8169e499a5d2f78218add710f0f53c5b  evidence/hermit-source-coverage.md
151ba2b0d17296b5e83efa15975b55ac61aed32f0008ff5912bc7f27b7ab2381  evidence/narf-facts.md
3867f91a36e5713d77a8b319abeaae518266bd563ab5f535c178b835605f3fa0  evidence/narf-findings.jsonl
87af8ecafaf2da777e80a7ee2a4c7560267b98ddb59648174efc31e358085cdc  evidence/narf-source-coverage.md
67ffe819a310930c492dbc026f5fd533f03663469afc056038a3bc3b9be1d6ef  evidence/reverie-backend-matrix.md
f718177e2d5468ec153e7673a22afbd666b56a40b9a47859bfbe788a7533f3a1  evidence/reverie-facts.md
646e88c56843dde7d6087447d0009da456cb0e4cf15db245cf750f566ba425aa  evidence/reverie-findings.jsonl
e4e9f70c114742644f4765b666a3e206c4ee6be06403da55a71196199e4c274a  evidence/reverie-source-coverage.md
1cc00d4bd3108d482387976c23aaddc16398f75110cbed33f54357794226bb94  evidence/trace-input-matrix.md
```

## Validation performed

- Read all fifteen pre-existing evidence files, the frozen contract, all 61
  JSONL records, the complete prior audit, and the canonical finding schema.
- `jq` parsed all three JSONL files; all 61 IDs are unique.
- Every one of 99 cited local file ranges exists at its mapped exact revision
  and fits within the cited file.
- The Hermit paired-query manifest's 22 stdout hashes reproduced with zero
  mismatches.
- Exact source and captured CI spot-checks reproduced every fact described in
  “Corrections independently reproduced.”
- `git diff --no-index --check /dev/null evidence/audit-a-lock.md` produced no
  whitespace diagnostics (exit 1 is the expected no-index “files differ”
  result). Running the same check over the complete untracked artifact set
  found one pre-existing defect: `research-contract.md:100` has a new blank
  line at EOF. No other artifact produced a whitespace diagnostic.

**Final verdict: CHANGES REQUIRED.**
