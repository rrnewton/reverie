# Audit J — final exact-head adversarial code review

## Verdict

**CHANGES REQUIRED.** The prior A2, A16, audit-history, and checklist findings
are closed, and the corrected scheduling design has a separate exact-head
**READY** verdict. One release-blocking evidence defect remains: the new
actionable-claim inventory still omits material current-state assertions that
change the security boundary and platform/POC scope, so the reported 33/33 trust
result does not yet prove the inventory's own completeness claim.

## Exact review target

- Repository/worktree:
  `/home/newton/work/dev-hermit/worktrees/slots/narf-hermit-design`
- Git base and current `HEAD`:
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`
- `design.md` SHA-256:
  `eaa877e81b067d63d0792771c6421c2bb3c4c1368a5a6757cc54b8071b818efd`
- `actionable-claim-inventory.md` SHA-256:
  `f21c0a4e405fffe875ed931594dc0d9d19638cd9b6f3023c9d5aa4ac9d47f128`
- `findings.jsonl` SHA-256:
  `8b3df1b38a8cceca4be54f1f29f4853ae72a7e5c9cf313876956cbd6b2b22bab`
- `verification.jsonl` SHA-256:
  `61a954e5bc5770abafec3cf64fe3e3a2cb1e5ba8da0574ab5b6ae885de73f6ef`
- `audit-resolution.md` SHA-256:
  `1688ac8ca3295a8955a722ece94b7a0df69fdacb207590b60706eb8144c2b179`
- `completion-checklist.md` SHA-256:
  `339f1bb73324b9b02bee90841dfa47e70a4ef06f4499a98915286815aad3fecb`
- Before this report was added, the sorted path-plus-content SHA-256 manifest of
  the 34-file, 652,177-byte package hashed to
  `0bb88c5d366f2d5fa54d6ce97e2593f1026272144ecb1eb2f6a4c4d038612ea1`.

All reviewed files were untracked additions relative to `HEAD`; no report commit
existed at review time.

## Finding

### Severity 1 — the 33-finding trust denominator still omits material current-state claims

`evidence/actionable-claim-inventory.md:5-11` defines its denominator as every
material current-state assertion that changes the architecture decision, POC
scope, comparator semantics, security boundary, or phase gate. The independent
verifier repeats that every decision-changing **Verified current** assertion and
current-state risk/table row maps to one of the 23 inventoried families
(`evidence/trust-gate-verifier-report.md:97-103`). That completeness assertion is
not true for the current design.

Material omissions include:

- `design.md:168` uses the existence of Narf verifier, JIT, maps, and domains as
  the current basis for the framekernel/BPF option. F-11/F-24/F-28 establish the
  syscall program type, test-run, and attach surface, while F-12/F-29 establish
  domain properties; none of the canonical claims establishes the current JIT,
  maps, or verifier ingredients named in this decision row.
- `design.md:504-510` and `design.md:517-519` state current Narf process/address-
  space isolation, guarded uaccess, BPF verifier and credential enforcement, and
  the exact BPF load-authority gate. These are expressly security-boundary facts,
  but the actionable inventory has no claim family or canonical finding for
  them.
- `design.md:649` says current Narf also has a mutable VFS and asynchronous
  network inputs. F-32 verifies hardware-derived time and CSPRNG only. VFS and
  network state determine the controlled-input scope but are outside the current
  canonical denominator.
- `design.md:687` reports the exact-commit aarch64 boot-smoke pass and full
  kernel-test failure and uses them in the platform disposition. F-25 is limited
  to the x86_64 ptrace/QEMU job. This executed platform evidence changes why
  aarch64 is deferred and is neither revision identity nor a no-runtime
  disclosure, the two classes the inventory says it checks separately.
- `design.md:135-136` states that the existing logger is synchronous serial
  output that perturbs latency. F-27 proves the absence of a generic attachable
  interposition trait, but does not establish those current logger properties.

The trust-gate script correctly joins every cited anchor to a verified record;
it cannot discover a current claim for which no anchor was added. Its
`verified_pct=1.0000` is therefore valid for the 33-record set but not evidence
that the set is complete. This is the same denominator class that Audit G asked
the new inventory to close, not a dispute with the facts themselves.

Required change: either narrow/remove each uncatalogued current assertion or add
canonical literal/operational findings and inventory rows for it, independently
verify the expanded stream, and rerun the fail-closed gate. The final verifier
must repeat the design-to-inventory completeness scan against the corrected
exact hashes. Do not solve this by broadening an existing finding's prose beyond
its evidence.

## Recheck of prior findings

- **A2 is closed.** `design.md:525-526`, `584`, and `613` distinguish frozen,
  characterized promotion cases from research-only cases. Promotion requires
  `matched`; unresolved/uncharacterized/unsupported/nonmatching cases cannot
  satisfy a gate.
- **A16 is closed.** `design.md:598-599` and `627` require two independent exact
  subclasses: family removal is `no_result: coverage`; truncation is
  `no_result: comparison_refused`. Generic `no_result`, semantic divergence, and
  `matched` are forbidden.
- **A0 is exact in the executable table.** Omitted exit expects the coverage
  subclass and a leaked zombie expects `infrastructure_error`
  (`design.md:582`).
- **Audit history is reconciled.** `evidence/audit-resolution.md` binds its map to
  the current design hash, preserves the historical failed reports, and maps
  every Audit C/D/E/F/G finding. The design appendix points to that map rather
  than rewriting history (`design.md:792-804`).
- **The substantive checklist is reconciled.** Research, source, coverage,
  design, and final-artifact rows are checked. Staging, commit, clean-state, and
  exact-final-diff rows correctly remain open until those operations occur
  (`evidence/completion-checklist.md:7-13`, `54-65`).
- **Scheduling review is current.** The separate deterministic readiness report
  returns **READY** for the exact design hash in
  `evidence/audit-i-final-determinism-ready.md`.

## Goalpost-moving assessment

- Assertions weakened: **no** — A2 now requires a frozen positive outcome and
  an exact match for promotion; uncharacterized cases remain nonpromoting.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no** — A16 is stricter than the prior generic refusal, all six A15 mutations
  remain, and A17/A18 retain their exact memory/instruction observations.
- Failure renamed or relabelled as a pass: **no** — unsupported, every no-result
  subclass, semantic divergence, and infrastructure failure remain distinct from
  `matched`.
- Check deleted instead of satisfied: **no** — historical failed audits,
  negative controls, the claim inventory, and the trust gate remain. The open
  finding is that the inventory check is incomplete, not that a check was
  deleted or an existing bar was lowered.

## Verification

- Read the complete current design, actionable-claim inventory, audit-resolution
  map, expanded verifier report, completion checklist, new canonical findings
  and verification records, prior final writing/determinism/adversarial reports,
  and current deterministic readiness verdict.
- Parsed all five JSONL files. The canonical stream has 33 unique IDs, 52
  evidence items, no unresolved dependency, and a one-to-one 33-record verifier
  stream with every tag `[VERIFIED]`.
- Confirmed the design has 38 anchor occurrences spanning F-1 through F-33,
  with no missing or unknown canonical ID.
- Re-ran the mandated trust-gate script using the current verifier stream. It
  returned `verified_pct=1.0000`, `coverage=1.0000`, `blocked=false`, and no
  failed finding. This validates the cited set, subject to the completeness
  finding above.
- Resolved every local Markdown link and found no broken target. Public source
  links remain commit-pinned where applicable; interactive browser testing is
  explicitly disclaimed.
- Ran whitespace/error checks over the complete pre-Audit-J package with no
  diagnostic.
- Rechecked exact parent, Hermit, nested and standalone Reverie, Narf, and Linux
  man-pages revisions. No source/runtime classification regression was found.
- No Hermit command, Narf/QEMU guest, provider runtime, commit, push, or
  publication occurred during this review.

## Limits

This is a design/evidence review, not runtime validation. I did not rerun Narf
CI or the raw discovery queries; their exact-revision receipts and Audit A
remain the evidence authority. A separate final writing audit was still active
when this verdict was written and must be considered independently. Any change
to the design, inventory, canonical findings, or verification stream requires a
fresh exact-head trust-gate and adversarial review.

**Final verdict: CHANGES REQUIRED.**
