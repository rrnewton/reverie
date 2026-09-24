# Completion and evidence checklist

This is an audit control document, not reader-facing design prose. Items remain
open until the named authoritative evidence exists and has been checked at the
recorded revision.

## Workspace and publication

- [x] Registered devbig030 agent slot `narf-hermit-design`, generation 1.
- [x] Branch `codex/narf-hermit-design` starts at recorded `origin/main`.
- [x] Only intentional report/evidence files are staged.
- [x] One local commit exists; no push/publication occurred.
- [x] Post-commit worktree is clean and commit content is re-read.

## Contract and source state

- [x] Decision, scope, exclusions, review sequence, and publication boundary frozen.
- [x] Narf repository and exact commit recorded and independently reproduced.
- [x] Parent, Hermit gitlink, and Reverie gitlink revisions recorded.
- [x] All public links are clickable and commit-pinned where possible.
- [x] Runtime evidence is explicitly distinct from source/test/comment evidence.

## Coverage controls

- [x] Complete Narf inventory: objects/lifecycle, control/data paths, ownership,
  ordering/errors, platform differences, consumers, implementation/tests.
- [x] Complete current Reverie backend inventory and provider matrix.
- [x] Paired precise/naive queries recorded for every research question.
- [x] Document classes searched or declared gaps.
- [x] Headings recorded for every opened document.
- [x] Seen-and-excluded sources ledger reviewed.
- [x] Inverse checks recorded.
- [x] Contradictions and missing evidence explicitly preserved.

## Required technical decisions

- [x] Current Narf ptrace status and exact required Hermit/Reverie semantics.
- [x] ptrace viability on current Narf.
- [x] seccomp/user-notification viability on current Narf.
- [x] ptrace-compatible emulation viability on current Narf.
- [x] first-class kernel interposition interface viability on current Narf.
- [x] framekernel/eBPF-like isolated-extension viability on current Narf.
- [x] feasibility/boundary of a new `reverie-narf` backend.
- [x] clean Rust syscall-interposition trait/extension boundary.
- [x] placement, isolation, lifecycle, memory, and syscall-data movement.
- [x] boot, architecture, executable, and userspace prerequisites.
- [x] scheduling/time/filesystem/randomness/signal/network input handling.
- [x] trace fields, normalization, matching, and ordering semantics.
- [x] failure/divergence taxonomy.
- [x] security and trust boundaries.
- [x] smallest POC, phased follow-ons, named interfaces, and open questions.
- [x] falsifiable executable acceptance tests with qualifying/refusing cases.

## Sequential audits

- [x] Audit A performed read-only by a separate correctness auditor.
- [x] Audit A canonical fact set, contradiction ledger, complete matrices,
  unresolved list, immutable facts, and denominator checks recorded.
- [x] Orchestrator reproduces every severity-0 fact and spot-checks every major
  subsystem/provider; Audit A explicitly locks facts before design drafting.
- [x] Audit B performed by a separate writing auditor using locked facts.
- [x] Audit B supplies answer, section architecture, claim homes, appendix split,
  table/diagram decisions, density budget, and review order without fact changes.
- [x] Substantive findings addressed without lowering evidence requirements.
- [x] Goalpost-moving assessment completed against the exact final diff.

## Final artifact

- [x] Claim labels are used consistently: Verified current, Proposed, Optional
  capability, Software helper, Above the API, Deferred, Unresolved.
- [x] Evidence appendix/coverage ledger and exact revisions included.
- [x] Recommendation is one smallest POC plus staged follow-ons.
- [x] Risks and remaining uncertainties are explicit.
- [x] Markdown links and internal anchors validate.
- [x] Final design contains no claim of unexecuted runtime support.
