# Final Audit A — correctness lock review

## Verdict

**CHANGES REQUIRED.** Audit A is not locked and Audit B must not begin. The
four previously named closure areas were re-audited against the corrected
evidence head. L-C1, L-C2, and L-C3 are closed. L-C4 is only partially closed:
the shared lane profile and separate final-schema boundary exist, but the
profile does not accurately enumerate all nonliteral snippet forms in the
current 99 `file_ref` records. A separate load-bearing current-state
contradiction also remains: two ledgers call Narf seccomp/filter-notifier
support unresolved although the pinned Narf implementation proves that filters
are not evaluated and `NEW_LISTENER` is rejected.

This was a fresh read-only correctness audit. The only file written by this
auditor is this report.

## Review target and source identity

- Registered slot: `narf-hermit-design`, generation 1, machine `devbig030`,
  branch `codex/narf-hermit-design`, purpose `Research and design
  Hermit-on-Narf POC`. `wrkslots status` reported consistent registry storage,
  the requested repository path, and start point
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`.
- Parent `dev-hermit`:
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`.
- [`rrnewton/hermit@ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2).
- Hermit's build-time
  [`rrnewton/reverie@ae1d1da78d2d89a1fa0454ee2282cf135e2286ed`](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed).
- Separately inventoried
  [`rrnewton/reverie@123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937).
- [`hodgesds/narf@afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/tree/afe919924a5fd2c8e820ab3a0564b6d45438e6bc).
- Linux man-pages:
  `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`.

All five local source trees were clean and matched those revisions.
`ae1d1da…` is seven commits after `123df7c…`; Hermit runtime facts continue to
use its actual build pin.

## Findings requiring correction

### Severity 0 — current Narf seccomp/filter-notifier status is still internally contradictory

[`hermit-facts.md:200`](hermit-facts.md) says it is **Unresolved** whether Narf
implements seccomp filters, listener FDs, and notifier ioctls.
[`reverie-source-coverage.md:200`](reverie-source-coverage.md) likewise leaves
the Narf seccomp-filter equivalent unresolved. The pinned primary source and
the current Narf fact set resolve the current-capability question:

- [`sys_seccomp.rs`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_seccomp.rs#L4-L55)
  stores mode `2` for both strict and filter mode but does not copy, store, or
  evaluate a filter; recognizing action constants does not execute them.
- The same handler rejects `SECCOMP_FILTER_FLAG_NEW_LISTENER` with `EINVAL`.
- Repository-wide precise and inverse searches found no notification object,
  notification ioctl lifecycle, or syscall-dispatch filter evaluator.
- [`narf-facts.md:137`](narf-facts.md) through line 155 and finding `N-F-12`
  already classify filter enforcement and user notification as absent at the
  pinned revision.

This is load-bearing for the interposition-option comparison. Current Narf
seccomp `RET_TRACE` and user notification are **Verified current (source-only)
absent/inert**, not unresolved. Restart, cancellation, memory-copy, and
lifecycle semantics for a *proposed future* notifier may remain **Unresolved**,
but that future contract must be separated from current support.

Required correction: reconcile both stale ledger statements with the Narf fact
set. Preserve the distinction between Linux `RET_TRACE` and `USER_NOTIF`; do not
convert either absence into runtime proof or future impossibility.

### Severity 1 — L-C4's shared profile omits four nonliteral snippet forms

[`finding-schema-notes.md`](finding-schema-notes.md) correctly names the shared
`narf-hermit-research-lane/v1` profile, applies it to all three lane files,
forbids canonical verification of preliminary nonliteral snippets, and requires
a separate `deep-research-finding-schema/v1` `findings.jsonl` plus verifier
output. That publication boundary is sound.

The profile says the only nonliteral forms are ellipsis-bearing or
whitespace-compacted paraphrases. An independent byte comparison of every
`file_ref.snippet` against its exact cited source range found:

- 99 total `file_ref` records;
- 69 literal contiguous snippets;
- 24 ellipsis-bearing snippets;
- 2 whitespace-only compacted snippets: `N-F-3` and `N-F-5`;
- 4 additional nonliteral snippets whose omitted source text or Markdown
  markup is not represented by an ellipsis and is not whitespace-only:
  `N-F-1`, `N-F-12`, `N-F-13`, and `N-F-20`.

The four additional cases are concrete:

- `N-F-1` omits the `rust-version` source line between `edition` and
  `license`.
- `N-F-12` omits the source comment inside the quoted `NEW_LISTENER` branch.
- `N-F-13` omits the multi-line comment between the listed BPF match arms and
  the wildcard arm.
- `N-F-20` removes Markdown blockquote markers and joins lines in the quoted
  stale documentation sentence.

All four still cite ranges that contain the relevant evidence, and none has
been given a canonical verifier verdict. The defect is the profile's claim to
have enumerated every deviation, not the underlying fact claims.

Required correction: either make those snippets literal, mark omissions with
an explicit ellipsis, or add and enumerate an `unmarked omission/markup
normalization` nonliteral category. Re-run the 99-reference byte check. Do not
weaken the final trust-gate schema or promote any lane receipt to verified.

## Closure review of L-C1 through L-C4

| Closure | Result | Reproduced evidence |
|---|---|---|
| L-C1 — LiteInst R-Q9 inversion | **Closed** | Current R-Q9 says the public Reverie `Backend` delegates to direct/in-guest `run_with_preload*`; separate `run_host_with_preload*` is the ptrace-host hybrid explicitly selected by Hermit. Exact source reproduced all three links. |
| L-C2 — Narf process-vm behavior versus Linux outcome | **Closed** | Current Narf behavior is source-verified: self/same-AS only, distinct-AS `EPERM`, gather before scatter, segment faults return `EFAULT`. Stock external Reverie bulk access is therefore incompatible. The exact paired Linux result for each page/segment fixture remains explicitly **Unresolved** rather than being called a proved divergence. |
| L-C3 — fourteen Reverie query pairs | **Closed** | R-Q1–R-Q14 each have a focused query and an exact two-word naive query, command/stdout/stderr/exit receipts, an explicit one-pair-per-question map, and a 115-entry integrity manifest. `sha256sum -c` passed. The manifest file hash is `b41b2f2906679c83715ed4cb40d62d86296412d2601af61f0c3b1e885ee69bb5`; all 28 stderr files are empty. An independent replay matched every receipt as a multiset of lines; byte order varied for threaded `rg`, so ordering was not misrepresented as semantic content. |
| L-C4 — shared lane profile/final schema | **Open** | The shared profile and separate final schema exist, but the claimed deviation inventory misses four unmarked omission/markup-normalized snippets and misidentifies `N-F-20` as whitespace-only while the actual second whitespace-only case is `N-F-5`. |

## Coverage, inventory, and primary-source spot checks

The following controls pass this review, subject to the two blocking findings
above:

1. The frozen contract states the decision, systems, dates/revisions, scope,
   exclusions, evidence bar, sequential audits, and local-commit-only boundary.
2. Narf records 13 focused/naive question pairs, document classes, complete
   headings for opened Markdown sources, heading-only exclusions for large
   narratives, seen-and-excluded sources, inverse checks, runtime/source
   separation, platform differences, and 2,956 tracked paths as an inventory
   denominator.
3. Hermit records 11 focused/naive pairs with 22 command/stdout/stderr/status
   receipts. Its manifest line/byte/hash data reproduced. Linux man-page and
   project-document headings, exclusions, inverse checks, and both Reverie pins
   are explicit.
4. Reverie records 14 focused/naive pairs, document classes, provider headings,
   exclusions, inverse checks, vendor revisions, and source-only runtime
   limits.
5. Exact source search yields three public Reverie `Backend` implementations:
   ptrace, LiteInst, and e9patch. It yields six execution-specific `Guest`
   implementations after excluding the generic `IntoGuest` adapter and test
   guest.
6. The 11-row provider/mechanism inventory covers ptrace, KVM, DBT, preload,
   LiteInst direct and ptrace-host paths, e9patch hybrid and direct paths,
   SaBRe/host, seccomp user notification, and excluded `riptrace`.
7. Source spot-checks reproduced placement, lifecycle, register/memory
   movement, ordering, and trust limits for every provider row. No package,
   callback registration, declaration, or test name was promoted to an
   executed backend pass.
8. Narf x86_64/aarch64 boot and syscall-stop differences, loader prerequisites,
   ptrace request/event gaps, domains, BPF, module lifecycle, scheduling, time,
   randomness, filesystem, network, signal, and security boundaries are
   inventoried.
9. The current-Narf option comparison covers stock ptrace, seccomp user
   notification, ptrace-compatible emulation, a first-class kernel
   interposition interface, and a framekernel/BPF-like extension.
10. The trace/input matrix separates current facts from **Proposed** and
    **Unresolved** contract material, preserves fail-closed comparison, and
    contains A0–A18: 19 proposed acceptance primitives, zero executed
    cross-kernel cells.

### Stock-incompatibility facts reproduced in primary source

- Narf ptrace is live and its exact-commit x86_64 QEMU log shows the named
  basic ptrace fixtures passing. The overall job still failed after QEMU exited
  with SIGSEGV at 457 seconds; no external Hermit/strace/gdb tracer ran.
- Stock Reverie requests seccomp and fork/clone/vfork/exec/exit events,
  `GETEVENTMSG`, `GETSIGINFO`, GPR/FP/XSTATE regsets, word memory,
  cross-address-space `process_vm_*`, single-step/syscall resume, and
  tracer-death containment. Current Narf lacks or diverges on several of these
  independently fatal requirements.
- Distinct-address-space `process_vm_readv/writev` returns `EPERM` on current
  Narf while stock Reverie invokes it from the distinct tracer process.
- Default fail-closed Detcore returns `Subscription::all()`, which includes
  CPUID and RDTSC. Reverie requests `PR_SET_TSC(PR_TSC_SIGSEGV)` and injects
  `ARCH_SET_CPUID(0)`. Narf accepts the former without disabling RDTSC and its
  FS/GS-only `arch_prctl` rejects the latter.
- Narf's seccomp filter mode is inert and `NEW_LISTENER` is rejected.
- Narf's BPF runtime has real verifier/map/fd/domain machinery and a sleepable
  syscall program type, but that type is test-run only; there is no guest
  syscall attach target or syscall trace-context ABI.
- Domain enforcement depends on the selected backend and tagged/mapped
  coverage. FRAME/domain 0 remains accessible, so hardware domains are defense
  in depth rather than a complete interposer sandbox.

## Terminology control

“Reverie public `Backend` implementation” and Hermit's narrower local
“backend” term are not interchangeable. Pinned Reverie really has
`impl Backend for E9patchBackend`; pinned Hermit classifies the CLI spelling as
e9patch preprocessing followed by the ptrace runtime. Audit B must therefore
write “e9patch preprocessing with the ptrace backend” for Hermit. It may not
call e9patch a Hermit backend. This terminology distinction does not alter the
three-implementation Reverie trait inventory.

## Contradiction disposition and immutable controls

The Narf/Reverie ledgers preserve the stale `linux-compat` feature prose, false
SEIZE documentation, ptrace-stub comments, MTE/domain/module documentation
drift, stale provider READMEs, accepted-but-inert option bits, and executed-CI
limits. No historical contradiction was silently deleted.

The next lock attempt must add and close these rows:

| ID | Severity | Conflict | Required disposition |
|---|---:|---|---|
| F-C1 | 0 | Hermit and Reverie coverage leave Narf seccomp/filter notification unresolved while Narf primary source proves filters inert and listener creation absent | Current support absent/inert; only the proposed future behavioral contract remains unresolved |
| F-C2 | 1 | Shared lane profile claims only ellipsis/whitespace-compacted nonliteral snippets, but four records contain unmarked omissions or markup normalization | Correct snippets or enumerate the third form; keep all nonliteral lane receipts ineligible for canonical verification |

Until they are closed, Audit B may not reinterpret these facts:

1. Narf has real ptrace, but current Narf does not support stock Reverie ptrace.
2. LiteInst's public/direct path and Hermit's selected ptrace-host hybrid are
   different paths.
3. E9patch is a public Reverie `Backend` implementation but is not a Hermit
   backend under Hermit's local definition.
4. Distinct-AS process-vm, lifecycle events/messages, signal provenance,
   CPUID/RDTSC interception, and cleanup gaps cannot be hidden by basic
   syscall-stop fixtures.
5. Narf's own process-vm gather/scatter behavior is verified source evidence;
   exact paired Linux page/segment outcomes remain unresolved.
6. Seccomp mode/action names do not prove filter execution or a notifier.
7. A BPF syscall program type does not prove syscall attachment.
8. Hardware domains do not replace verifier/helper safety.
9. Return/errno equality without moved bytes, persistent effects, lifecycle,
   ordering, and complete coverage is not functional equivalence.
10. Record/replay fidelity is not independent Linux/Narf equivalence.
11. Proposed trace normalization and A0–A18 are not runtime support.
12. A declaration, test, comment, accepted option, callback registration, or
    zero exit is not an executed capability result.

## Denominator check

| Figure | Denominator and admissible meaning |
|---|---|
| 2,956 Narf paths | All tracked paths at the pinned commit; not files read and not capabilities |
| Query hit counts | Matching output lines; not unique files, capabilities, or support rates |
| 3 public Reverie `Backend` impls | All production Rust `impl Backend for` blocks at `123df7c…`; excludes generic/test adapters and helpers |
| 6 execution-specific `Guest` impls | Concrete provider guests; excludes the generic `IntoGuest` and `TestGuest` |
| 11 provider/mechanism rows | Includes split direct/hybrid rows and excluded `riptrace`; not eleven conforming Hermit backends |
| 61 lane records | 23 Narf + 18 Reverie + 20 Hermit; all IDs unique; none is the final canonical `F-N` stream |
| 99 lane `file_ref` records | 69 literal, 24 ellipsis-bearing, 2 whitespace-only compacted, 4 unmarked omission/markup-normalized |
| 115 Reverie manifest entries | Runner/query/map inputs plus 112 per-query receipt files; integrity entries, not findings |
| 16 Narf domain IDs | Reserved namespace; not sixteen runtime-demonstrated enforcement domains |
| Provider test/ignore counts | Matching source files/annotations; not executed cases or pass rates |
| A0–A18 | Nineteen proposed acceptance IDs; zero executed cross-kernel cells |

No current-support percentage or provider success rate is admissible from
these figures.

## Runtime/source reconciliation

This auditor executed no product runtime test, QEMU guest, or Hermit command.
No ad-hoc Hermit invocation occurred, so `safehermit` was not needed.

The only executed product evidence in the research set is captured remote CI
from Narf's exact commit. GitHub job `107842987657` visibly reports the named
x86_64 ptrace fixtures and two process ptrace smokes as `[ OK ]`, then reports
`qemu-system-x86_64` exiting with SIGSEGV after 457 seconds; the job conclusion
is failure. Other cited job results are exact-head captured API records.
Reverie/Hermit provider conclusions remain source-only.

## Goalpost-moving assessment

- Assertions weakened: **no**. No product or test assertion was edited. The
  current evidence adds A17/A18 rather than weakening the required stock-memory
  and instruction-interception checks.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. A15/A16 still require negative-control sensitivity and fail-closed
  refusal for missing/truncated coverage.
- Failure renamed or relabelled as a pass: **yes, in one evidence gate**. L-C4
  presents its deviation inventory as complete while four omission-shaped
  snippets do not fit either named category. None was promoted to verified,
  but the classification makes the schema closure appear satisfied and blocks
  a correctness lock.
- Check deleted instead of satisfied: **no**. Both historical
  `CHANGES REQUIRED` audits, the open completion checklist, query receipts,
  negative controls, and trust-gate separation remain present.

There is no evidence that a test or semantic comparator was deliberately
lowered. This audit refuses to treat relabeling or an overly broad
“Unresolved” status as closure.

## Validation performed

- Read the frozen contract, protocol, code-review policy, every tracked evidence
  artifact, both historical `CHANGES REQUIRED` audits, all 61 current JSONL
  findings, the raw query manifests/receipts, and exact-revision primary source.
- Reconciled the registered slot and all source-tree revisions.
- Parsed all JSONL with `jq`; all 61 IDs are unique and all dependencies resolve.
- Verified that every one of 99 file ranges exists at its mapped revision and
  fits the cited file, then compared every snippet byte-for-byte and under a
  whitespace-only normalization.
- Reproduced all 115 Reverie receipt checksums and independently replayed all
  28 searches; replay content matched after sorting `rg`'s threaded output.
- Reproduced all 22 Hermit stdout receipt hashes, statuses, and line counts.
- Spot-checked every provider/platform row and every stock-incompatibility fact
  named above against local exact-revision source.
- Re-read exact-head Narf CI records and the x86_64 job log.
- `git diff --no-index --check` over every pre-existing tracked artifact
  produced no whitespace diagnostic.

## Reviewed input hashes

The verdict is bound to these exact pre-existing tracked inputs:

```text
ba01e11a74dcc798363a0e648b0fb25c764595bb750163dadc4baac645ba57e3  research-contract.md
ec741e902f4d9a25a4ed0563c20b8398006c50f03190002d2af8ba8558aecc77  evidence/README.md
c0e89d56f1a6a69f0dc3d9a0105f0a87ffadcf2ab50c317bdff947ce9654eb56  evidence/audit-a-lock.md
eae5c53842e26ad6bf7a8df56456031ced62d1e880f790f7f879b208eb73de9b  evidence/audit-a.md
def65e554a23c81239c29b71d669b00b1c87389e6a6b533d9e7fdd01e29c5589  evidence/completion-checklist.md
22a1ab12184156d2a0814804999dd9b74a92b9c53aaaa728625789f1e1a1f5c8  evidence/finding-schema-notes.md
105568d678dbf5f5ebfe0acaa5ee4761765519716598f13de384f708551dbac5  evidence/hermit-facts.md
ff4c021c621c3631a7109ea92c5d98ef4c1ddc832f25ab29a757909a08d3ade2  evidence/hermit-findings.jsonl
4aca5282e782a676260204985a947ada8169e499a5d2f78218add710f0f53c5b  evidence/hermit-source-coverage.md
e060b30bfdab6edb0bba904e73656bdf8da22b656533ad28548760c30f523f87  evidence/narf-facts.md
2dc93dad4bfd789b5fc7e003d8b0fe7092dd48fd47edc77e30580775d8fc1c76  evidence/narf-findings.jsonl
87af8ecafaf2da777e80a7ee2a4c7560267b98ddb59648174efc31e358085cdc  evidence/narf-source-coverage.md
67ffe819a310930c492dbc026f5fd533f03663469afc056038a3bc3b9be1d6ef  evidence/reverie-backend-matrix.md
f718177e2d5468ec153e7673a22afbd666b56a40b9a47859bfbe788a7533f3a1  evidence/reverie-facts.md
646e88c56843dde7d6087447d0009da456cb0e4cf15db245cf750f566ba425aa  evidence/reverie-findings.jsonl
43b232428f6c57685b096665df3b879e2e8ee66b8b9e47c4cd86de60270fa9d4  evidence/reverie-source-coverage.md
1cc00d4bd3108d482387976c23aaddc16398f75110cbed33f54357794226bb94  evidence/trace-input-matrix.md
```

Selected raw-evidence integrity anchors:

```text
b41b2f2906679c83715ed4cb40d62d86296412d2601af61f0c3b1e885ee69bb5  reverie/query-pairs-v1/manifest.sha256
253b54fceece3db2bcb5dce97666c1c456bae29bc185105bccd131e1a1ba4d62  hermit/paired-queries-20260924/MANIFEST.txt
5b1291e92323a356ea245a98b2946f1ee79da5c6b8fe32c374ff82598820297c  narf/query-results.txt
0bcd75998b9a0b4d61c2aa5a7f06d0a3e823f56aa4189814e949dd3c17389d2d  narf/audit-a-correction-queries.txt
8e816cfdff7c65a33ea36005d7caf99694a2f23314ff8aaff040ca38d671f1d2  narf/check-runs.json
0002f3dddb330d012e0aca4a3993d5d6185f67b2d82c393e718dd1f6c1d9ca64  narf/kernel-test-x86.log
```

**Final verdict: CHANGES REQUIRED.**
