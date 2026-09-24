# Hermit-on-Narf POC research contract

Status: frozen before evidence collection and reader-facing drafting.

## Decision

Choose the Reverie/Hermit backend architecture that can most feasibly support a
first proof of concept comparing the functional behavior of the same Linux ABI
guest program on Linux and on the Narf kernel. Define the implementation and
falsifiable tests needed to prove that both executions yield the same canonical
observable trace.

## In scope

- Current public Narf implementation and documentation at one recorded commit.
- `rrnewton/hermit` and `rrnewton/reverie` at exact recorded commits.
- Every current Reverie execution backend and its demonstrated, source-only, or
  absent capabilities.
- Narf's current ptrace implementation status and the exact ptrace semantics
  used by Hermit/Reverie.
- The feasibility and boundary of a new `reverie-narf` backend.
- A clean Rust syscall-interposition trait or extension boundary.
- ptrace, seccomp/user notification where relevant, ptrace-compatible
  emulation, a first-class in-kernel syscall-interposition interface, and a
  framekernel/eBPF-like isolated extension.
- Extension placement, isolation, lifecycle, memory access, syscall-data
  movement, security, and trust boundaries.
- Boot, architecture, and executable prerequisites.
- Scheduling, time, filesystem, randomness, process, signal, and network
  inputs that affect equivalence.
- Canonical trace definition and normalization; failure and divergence classes.
- A phased implementation plan and executable, falsifiable acceptance tests.

## Out of scope

- Implementing any backend or kernel feature.
- Claiming runtime support that was not executed and observed.
- Production hardening beyond identifying the trust and isolation work.
- Performance equivalence; performance may be measured only to establish POC
  practicality.
- Publishing a GitHub issue, pull request, comment, branch, or push.

## Source state

- Parent `dev-hermit` worktree start: `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`
  (`origin/main` when the registered worktree was created on 2026-09-24).
- Parent gitlinks at that revision:
  - `hermit`: `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`
  - `reverie`: `123df7c4c0169006fbfe1f11a1553fe333eac937`
- Narf revision: unresolved until the public repository is identified and
  cloned under `ignored/narf-hermit-poc-design-20260924/`.
- Runtime evidence and source inspection are recorded separately. Declarations,
  comments, target names, allocation paths, and test existence do not prove
  runtime support.

## Required primary sources and comparisons

- Commit-pinned Narf implementation, tests, build/boot documentation, and public
  interface documentation.
- Commit-pinned Hermit and Reverie implementation, tests, manifests, and public
  documentation.
- Linux man-pages or kernel documentation for ptrace and seccomp user
  notification semantics used as comparison contracts.
- Source and evidence for each current Reverie backend, not only workspace
  package names.
- Both precise and naive search queries for every research question, plus
  inverse checks and a ledger of read-but-excluded sources.

## Required inventories and matrices

- Narf public objects, lifecycle, control path, data path, resources, ordering,
  visibility, error semantics, platform differences, consumers, implementation
  status, and test evidence.
- Reverie backend inventory and provider-by-capability matrix.
- Interposition-option matrix covering placement, lifecycle, stop semantics,
  register/memory access, concurrency, security, required kernel changes, and
  POC feasibility.
- Trace-field and normalization matrix.
- Acceptance-test and failure-class matrix.

## Review and publication boundary

1. Evidence collection may run in parallel.
2. A separate read-only correctness auditor must review source coverage, lock a
   canonical fact set, and close or preserve contradictions before any
   reader-facing design prose is drafted.
3. The orchestrator must independently reproduce severity-0 facts and spot-check
   every major subsystem/provider before accepting Audit A.
4. A separate writing auditor receives the locked facts as immutable input and
   proposes structure; it may not alter facts or resolve open decisions.
5. Substantive findings are corrected without weakening evidence requirements.
6. Publication stops at one local commit in this registered worktree. No push,
   issue, pull request, comment, or other publication is authorized.

## Claim labels

Every material claim in the fact set and final design is explicitly labeled as
one of: **Verified current**, **Proposed**, **Optional capability**,
**Software helper**, **Above the API**, **Deferred**, or **Unresolved**.
