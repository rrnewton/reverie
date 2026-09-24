# Actionable-claim inventory

Status: final trust-gate denominator control.

Method: enumerate every material current-state assertion that changes the
architecture decision, POC scope, comparator semantics, security boundary, or
phase gate. Each row must cite at least one canonical `F-N` finding used by
`design.md`. Proposed interface/test requirements are normative design choices,
not source claims; they are reviewed by the writing, deterministic-scheduling,
and adversarial audits instead. Exact-revision identity and no-runtime
disclosures are checked separately in the revision/platform/runtime tables.

| Current actionable claim family | Canonical finding(s) | Design home |
|---|---|---|
| Reverie arbitrary-Tool/process-tree conformance contract | F-1, F-19 | Conformance boundary |
| Exactly three public production `Backend` implementations | F-2 | Conformance boundary/provider inventory |
| LiteInst direct trait path vs Hermit ptrace-host hybrid | F-20, F-21 | Terminology/provider inventory |
| E9patch public trait path is tracer-backed | F-22 | Terminology/provider inventory |
| Stock ptrace option/event and selective seccomp-TRACE mechanism | F-3, F-33 | Current ptrace/interposition table |
| Stock ptrace distinct-AS bulk-memory mechanism | F-4 | Current ptrace/interposition table |
| Default Detcore subscriptions include the full set | F-5 | Current ptrace blockers |
| Narf TSC/CPUID instruction-interception gaps | F-6, F-7 | Current ptrace blockers/platform matrix |
| Narf real but incomplete ptrace request/event surface | F-8, F-26 | Current ptrace/interposition table |
| Exact-commit bounded Narf ptrace execution and later QEMU failure | F-25 | Evidence boundary/current ptrace/runtime reconciliation |
| Narf seccomp filter/listener absence/inertness | F-9 | USER_NOTIF decision row |
| Narf live dispatcher and absence of a generic typed interposition API | F-10, F-27 | Recommended seam |
| Narf distinct-address-space `process_vm_*` rejection | F-23 | Current ptrace blockers/A17 |
| Narf ELF/PT_INTERP loader baseline | F-14 | Boot/fixture prerequisites |
| Narf BPF syscall type/test-run vs actual attach surface | F-11, F-24, F-28 | BPF decision/security boundary |
| Narf conditional domain backend and writable FRAME boundary | F-12, F-29 | BPF/security boundary |
| Narf checked/guarded user-memory copy boundary | F-34 | Security/data-movement boundary |
| Narf BPF per-task credential load authority vs bootstrap capability | F-35 | Security boundary |
| Narf ptrace attach target/credential checks | F-37 | Security boundary |
| Narf per-CPU/work-stealing scheduler baseline | F-13 | Scheduling risk/platform scope |
| No Hermit-style deterministic/virtual-time/replay implementation found in Narf | F-31 | Evidence boundary/deferred replay |
| Narf hardware-derived time and CSPRNG input | F-32 | Controlled-input risk |
| Hermit canonical log discipline and I/O-buffer comparison setting | F-15, F-30 | Trace contract |
| Hermit typed verdict independent of guest exit | F-16 | Outcome taxonomy |
| Hermit record/replay is one-recording replay fidelity, not independent equality | F-17 | Trace/replay boundary |
| Linux seccomp USER_NOTIF blocking/response/restart semantics | F-18 | USER_NOTIF decision row/A8 |
| Exact-commit aarch64 boot-smoke success vs kernel-test failure | F-36 | Platform matrix |
| Non-`Backend` KVM/DBT/preload/SaBRe/notifier helper inventory | F-38 | Complete provider inventory |

Completeness check: all 38 IDs are referenced in `design.md`; no unknown ID is
referenced. The independent verifier must verify every canonical record, and
the trust gate must report both cited-anchor coverage and 100% verified status.
Any new material current-state assertion requires a new or expanded canonical
finding plus a fresh independent verification; changing only prose cannot
silently expand this denominator.
