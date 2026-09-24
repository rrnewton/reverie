# Reverie source coverage ledger

## Scope and revision

- Question: inventory every current Reverie backend/provider and establish its real execution path, extension seams, and exact stock-ptrace kernel dependencies relevant to a Narf intersection.
- Repository: [`rrnewton/reverie`](https://github.com/rrnewton/reverie).
- Exact revision: [`123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937), commit date 2026-09-20.
- Collection time: 2026-09-24T21:54:14Z.
- Raw query output: `ignored/narf-hermit-poc-design-20260924/reverie/` in the registered worktree. Raw output is intentionally untracked.
- Runtime policy: source-only. No Hermit invocation occurred. No backend runtime test occurred.
- Finding-ID convention: this evidence shard intentionally uses task-scoped `R-FN` identifiers, as required by the dispatch, instead of the deep-research schema's team-global `F-N`. All other finding fields follow the canonical schema; the orchestrator must map IDs if it builds one team-global stream.

## Query ledger

Each question has one newly replayed focused query and one exact two-word naive query. All use this fixed command shape from the exact Reverie checkout:

`rg -n --hidden --glob '*.rs' --glob '*.c' --glob '*.md' --glob 'Cargo.toml' --glob '!third-party/**' --glob '!reverie-dbt/vendor/**' -- '<query>' .`

For every receipt basename below, `.command` records the shell-quoted command, `.stdout` and `.stderr` record the two streams, and `.exit` records the status. The receipt root is `ignored/narf-hermit-poc-design-20260924/reverie/query-pairs-v1/`. No receipt is reused between questions; `receipt-map.tsv` maps all 14 pairs explicitly.

### R-Q1 — public backend implementations

- Focused query: `impl[[:space:]]+Backend[[:space:]]+for`
- Naive query: `backend implementation`
- Receipts: `R-Q1-focused.*`, `R-Q1-naive.*`; exits `0`, `0`.
- Result: exactly ptrace, LiteInst, and e9patch implement `Backend`.

### R-Q2 — execution-specific Guest adapters

- Focused query: `impl([^[:cntrl:]]*[[:space:]])?Guest<[^>]+>[[:space:]]+for|impl<[^[:cntrl:]]+>[[:space:]]+Guest<`
- Naive query: `guest adapter`
- Receipts: `R-Q2-focused.*`, `R-Q2-naive.*`; exits `0`, `0`.
- Result: execution-specific adapters are TracedTask, KvmGuest, DbtGuest, LiteinstGuest, E9patchGuest, and SabreGuest.

### R-Q3 — generic Tool/Guest callback boundary

- Focused query: `fn (handle_syscall_event|handle_signal_event|handle_thread_start|handle_post_exec|handle_timer_event|handle_cpuid_event|handle_rdtsc_event)|async fn (inject|tail_inject)|trait (Tool|Guest|GlobalTool)`
- Naive query: `tool callback`
- Receipts: `R-Q3-focused.*`, `R-Q3-naive.*`; exits `0`, `0`.
- Result: the generic boundary is Tool/Guest/GlobalTool/Subscription/MemoryAccess.

### R-Q4 — exact ptrace protocol

- Focused query: `PTRACE_(O_|EVENT_|GET|SET|SYSCALL|SINGLESTEP|CONT|TRACEME|PEEK|POKE)`
- Naive query: `ptrace protocol`
- Receipts: `R-Q4-focused.*`, `R-Q4-naive.*`; exits `0`, `1` (valid zero-hit naive control).
- Result: the option/event/request surface is captured in the backend matrix; seccomp-event delivery is the primary syscall-entry path.

### R-Q5 — restart and signal rules

- Focused query: `ERESTART|EINTR|restart_syscall|getsiginfo|setsiginfo|peeksiginfo|pending_signal`
- Naive query: `signal restart`
- Receipts: `R-Q5-focused.*`, `R-Q5-naive.*`; exits `0`, `1` (valid zero-hit naive control).
- Result: injected-syscall interruption/retry and ordinary signal reinjection paths are identified; no normal `SETSIGINFO` consumer was found.

### R-Q6 — KVM execution path

- Focused query: `struct KvmBackend|struct KvmGuest|trait (SyscallExecutor|GuestSyscallExecutor)|run_static_elf`
- Naive query: `kvm backend`
- Receipts: `R-Q6-focused.*`, `R-Q6-naive.*`; exits `0`, `0`.
- Result: KVM is a specialized static-ELF/bare-personality runner and does not implement public `Backend`.

### R-Q7 — DynamoRIO DBT execution path

- Focused query: `DbtRunner|DbtGuest|drmgr_register_(pre_syscall|post_syscall|signal|thread|module)|event_(signal|thread)`
- Naive query: `dbt backend`
- Receipts: `R-Q7-focused.*`, `R-Q7-naive.*`; exits `0`, `1` (valid zero-hit naive control).
- Result: DBT is an in-process DynamoRIO provider without a public `Backend` implementation; its ABI restrictions are separately source-verified.

### R-Q8 — preload boundary

- Focused query: `SyscallDispatcher|SECCOMP_RET_TRAP|SIGSYS|raw_syscall6|LifecycleController`
- Naive query: `preload interception`
- Receipts: `R-Q8-focused.*`, `R-Q8-naive.*`; exits `0`, `1` (valid zero-hit naive control).
- Result: preload is a reusable in-process primitive, not a `Backend`, with explicit coverage gaps.

### R-Q9 — LiteInst direct and ptrace-host paths

- Focused query: `impl Backend for LiteinstBackend|run_with_preload|run_host_with_preload|launch::<T>`
- Naive query: `liteinst backend`
- Receipts: `R-Q9-focused.*`, `R-Q9-naive.*`; exits `0`, `0`.
- Result: the public `Backend` is the bounded direct/in-guest `run_with_preload*` path; `run_host_with_preload*` is a separate optional ptrace-host hybrid explicitly selected by Hermit.

### R-Q10 — e9patch paths

- Focused query: `impl Backend for E9patchBackend|run_direct|spawn::<T>|rewrite|E9PATCH`
- Naive query: `e9patch backend`
- Receipts: `R-Q10-focused.*`, `R-Q10-naive.*`; exits `0`, `0`.
- Result: e9patch's public `Backend` is the ptrace-hosted hybrid; its direct path is separate and lifecycle-incomplete.

### R-Q11 — SaBRe path

- Focused query: `ReverieAdapter|SabreGuest|into_sabre|REVERIE_SOCK|tail_inject`
- Naive query: `sabre backend`
- Receipts: `R-Q11-focused.*`, `R-Q11-naive.*`; exits `0`, `0`.
- Result: SaBRe/reverie-host is an experimental adapter/launcher without a public `Backend` implementation.

### R-Q12 — seccomp user notification

- Focused query: `NEW_LISTENER|SeccompNotif|SECCOMP_IOCTL_NOTIF|seccomp_notify`
- Naive query: `user notification`
- Receipts: `R-Q12-focused.*`, `R-Q12-naive.*`; exits `0`, `1` (valid zero-hit naive control).
- Result: the generic process helper and source test exist; no execution `Backend` consumes the helper.

### R-Q13 — test/runtime evidence

- Focused query: `#\[(tokio::)?test\]|#\[ignore|validate\.sh|runtime_tests`
- Naive query: `runtime test`
- Receipts: `R-Q13-focused.*`, `R-Q13-naive.*`; exits `0`, `0`.
- Result: source test material is inventoried; no test declaration was promoted to executed support.

### R-Q14 — inverse provider inventory

- Focused query: `impl Backend for|impl([^[:cntrl:]]*[[:space:]])?Guest<|drmgr_register_pre_syscall|SyscallDispatcher|SeccompNotif|ReverieAdapter`
- Naive query: `execution provider`
- Receipts: `R-Q14-focused.*`, `R-Q14-naive.*`; exits `0`, `1` (valid zero-hit naive control).
- Result: no additional execution provider was found; riptrace is a frontend and support crates are helpers.

The integrity manifest `query-pairs-v1/manifest.sha256` has 115 entries covering the runner, query/map inputs, and every command/stdout/stderr/exit receipt. Its SHA-256 is `b41b2f2906679c83715ed4cb40d62d86296412d2601af61f0c3b1e885ee69bb5`. All 28 stderr files were empty. Earlier discovery files such as `query-backend-impls.txt`, provider-specific files, inverse-query files, `tree.txt`, `packages.txt`, and `test-inventory.txt` remain raw historical receipts; the complete reproducible pairs above supersede them for R-Q1–R-Q14.

## Document classes covered

| Class | Coverage and use |
|---|---|
| Implementation source | Primary current evidence for every behavior claim. |
| Public trait/API source | Backend/Tool/Guest/Subscription contract and typed syscall boundary. |
| Cargo/workspace manifests | Exhaustive package inventory, dependencies, LiteInst git revision. Not behavior proof. |
| Source tests and validation scripts | Coverage discovery and test inventory only. Not treated as pass evidence. |
| Current provider READMEs | Prerequisites and explicit scope discovery; checked against implementation before use. |
| Historical assessments/AI docs | Seen, but excluded from current behavior facts unless independently reproduced in current source. |
| Vendored source/submodule metadata | Exact provider revisions and native callback behavior. |
| Runtime output | None; absence explicitly recorded. |
| Issues/PR prose and external discussions | Not needed to establish pinned current source behavior; not searched for normative facts. |
| Narf source | Owned by the Narf evidence researcher; only their supplied intersection is labeled as cross-thread input here. |

## Opened headings and source surfaces

| Source | Opened headings/symbols | Evidence role |
|---|---|---|
| `reverie/src/backend.rs` | “The contract”, “A note on Send”, `Backend` | Normative generic backend obligations. |
| `reverie/src/tool.rs` | `Tool`, subscriptions, lifecycle callbacks, syscall/signal/timer hooks | Tool policy and lifecycle boundary. |
| `reverie/src/guest.rs` | memory, regs/set_regs, inject/tail_inject, retry, timers/clock | Kernel-facing operations backends must provide. |
| `reverie/src/subscription.rs` | event set and syscall iterator | Current event subscription vocabulary. |
| `reverie-ptrace/src/{backend,tracer,task}.rs` | launch, setoptions, seccomp filter, event dispatch, skip/inject/restart/signal/exit | Stock ptrace execution path. |
| `safeptrace/src/{lib,memory,waitid,notifier}.rs` | event decoding, register requests, resume methods, guest memory, wait semantics | Exact ptrace ABI consumption. |
| `BACKENDS.md` | `Reverie backend architecture`; `Contract status`; `Mechanism matrix`; `Shared components`; `[COMPONENT:RPC]`; `[COMPONENT:PTRACER]`; `[COMPONENT:TRAPPING]`; `e9patch and SaBRe: sharing and performance` | Discovery only because embedded links target an older source revision. |
| `reverie-kvm/README.md` | `reverie-kvm`; `ELF execution`; `Vectored I/O`; `Bounded signal delivery`; `Task names and prctl state`; `Typed syscall decoding`; `CPUID policy`; `Relationship to gVisor`; `Current limits` | KVM scope discovery, checked against implementation. |
| `reverie-kvm/src/{lib.rs,vm.rs,runtime.rs}` | loader/vCPU, KvmGuest, SyscallExecutor, runner symbols | KVM placement and bounded capability. |
| `reverie-dbt/README.md` | `Reverie DynamoRIO Backend Prototype`; `Build`; `Prototype Boundaries`; `Third-party licenses` | DBT scope discovery; stale boundaries logged below. |
| `reverie-dbt/{src/launcher.rs,src/lib.rs,native/client.c}` | launcher, DbtGuest, native registrations/pre-post syscall/signal/thread events | DBT real execution path. |
| `reverie-preload/README.md` | `reverie-preload`; `What it provides`; `Coverage boundaries`; `Two ways to use it`; `Hybrid-ptrace boundary`; `Migration note for existing backends` | Shared-runtime scope discovery; stale migration statement logged below. |
| `reverie-preload/src/{lib.rs,dispatch.rs,trap.rs,lifecycle.rs}` | SyscallEvent/Dispatcher, SIGSYS trap, lifecycle controller | Shared in-process primitive. |
| `reverie-liteinst/README.md` | `Reverie LiteInst`; `Event path`; `Unpatchable syscalls`; `Backend launcher`; `Shared reverie-preload built-in tools`; `Shared reverie-preload runtime configuration`; `Patch publication modes`; `Current boundaries`; `Fallback-surface observability`; `Corpus sweep scorecard`; `Caution: --verify cannot detect an ignored clone/fork rejection` | LiteInst scope discovery, including direct-path limits. |
| `reverie-liteinst/src/{backend.rs,tool_host.rs,runtime.rs}` | direct `Backend` impl, separate ptrace-host APIs, fail-closed guards | LiteInst current paths. |
| `reverie-e9patch/README.md` | `reverie-e9patch`; `Hybrid Backend`; `Preparation`; `Shared With LiteInst Versus Different`; `Fallback-Surface Observability`; `Preload And RPC Boundary`; `Toolchain` | e9patch scope discovery. |
| `reverie-e9patch/src/{backend.rs,rewrite.rs,tool_host.rs,aot.rs}` | hybrid Backend impl, direct APIs, artifact validation, callback | e9patch paths. |
| `experimental/reverie-sabre/CAPABILITIES.md` | `reverie-sabre capabilities`; `Supported runtime behavior`; `Conformance gate`; `Shared example-tool matrix`; `Known limitations` | Current scope discovery, checked against implementation. |
| `experimental/reverie-sabre/ASSESSMENT.md` | `reverie-sabre backend assessment`; `Verified functionality`; `Backend model`; `Current limitations` | Historical document, excluded from current facts. |
| `experimental/reverie-sabre/src/{reverie_adapter.rs,utils.rs}` | adapter callbacks, Guest impl, exec utility | Current SaBRe implementation. |
| `experimental/reverie-host/src/tracer.rs` | host launch/RPC | SaBRe controller placement. |
| `reverie-process/src/seccomp/{bpf,notif}.rs`, builder/spawn/test | NEW_LISTENER, recv/send/id-valid/addfd, FD lifecycle | User-notification helper. |
| Hermit `detcore/src/lib.rs`, `hermit-cli/src/lib.rs` | default `Subscription::all()` and explicit LiteInst `run_host_with_preload` dispatch symbols | Cross-repository Audit A correction at exact Hermit revision `ba9c4f4...`; no Hermit README was opened by this worker. |

## Seen-and-excluded ledger

| Source/item | Seen | Exclusion or downgrade reason |
|---|---:|---|
| Root `BACKENDS.md` | Yes | Discovery only: its public source links are pinned to older `2f812...`, so current implementation source overrides it. |
| Provider package name/description | Yes | Inventory clue only; never used to infer execution completeness. |
| `#[test]`, integration test file, or validation entry | Yes | Proves a test exists, not that it passed or that declarations execute. |
| `reverie-dbt/README.md` older prototype limits | Yes | Several limits are contradicted by newer native implementation; current source wins. Remaining source-observable limitations retained. |
| `reverie-preload/README.md` migration statement | Yes | Says LiteInst/e9patch retained copies, contradicted by current dependency/source reuse. Excluded as stale. |
| LiteInst comments saying unpatchable fallback absent | Yes | Current deferred callback source implements a path; stale comment excluded. |
| SaBRe `ASSESSMENT.md` | Yes | Dated historical assessment, excluded from current facts. |
| SaBRe CAPABILITIES claim that `execveat` is absent | Yes | Current `utils.rs` has `execveat` re-entry; source wins. |
| Third-party DynamoRIO/e9patch/SaBRe docs | Yes | Used only when current adapter/native source linked the mechanism; upstream aspiration excluded. |
| `experimental/riptrace` | Yes | Frontend/tool, not another syscall-interposition backend. |
| `reverie-memory`, `reverie-process`, RPC/syscall crates | Yes | Shared helpers, not standalone providers unless a real launch/execution path consumed them. |
| Seccomp user-notification source test | Yes | Validation script skips it and this investigation did not run it; helper existence only. |
| Narf ptrace declarations/tests/comments | Not independently reopened here | Narf researcher owns the broad ptrace inventory; supplied intersection remains visibly qualified. |
| Narf `core.inc.rs`, `sys_prctl.rs`, `sys_arch_prctl.rs` | Yes, exact revision | Independently opened only for Audit A's cross-AS `process_vm_*`, RDTSC, and CPUID corrections; source-only facts are pinned in the fact set. |

## Inverse checks

1. Searched all Rust/C source for `impl Backend for`: only ptrace, LiteInst, and e9patch.
2. Searched all Rust source for `impl ... Guest`: only six execution-specific guests listed in the matrix.
3. Searched all workspace packages and binaries for launch/interception paths; no hidden provider beyond KVM, DBT, preload, LiteInst, e9patch, SaBRe/host, ptrace, and the notification helper.
4. Searched raw `PTRACE_*` and wrapper calls, not just high-level `safeptrace` methods, to avoid missing regsets/options/events.
5. Searched for absent ptrace features (`SEIZE`, `INTERRUPT`, `LISTEN`, `GET_SYSCALL_INFO`, `SETSIGINFO`) and distinguished wrapper existence from live call-site use.
6. Searched both `SECCOMP_RET_TRACE` and user-notification actions; did not collapse distinct seccomp mechanisms.
7. Checked current implementation against README claims and logged contradictions rather than accepting prose.
8. Searched `process_vm_*` separately from ptrace word access to expose the bulk-memory dependency.
9. Enumerated native vendor revisions recursively and recorded exact SHAs.

## Gaps carried to Audit A

- **Unresolved:** no bounded runtime result for any provider at this revision.
- **Unresolved:** public GitHub line anchors are commit-pinned but were not fetched through a browser; local exact-revision source was the byte source.
- **Unresolved:** no Narf execution was performed by this worker. Exact Narf source was independently opened only for cross-AS `process_vm_*`, RDTSC, and CPUID behavior.
- **Verified current (source-only):** pinned Narf does not evaluate seccomp
  filters, rejects `SECCOMP_FILTER_FLAG_NEW_LISTENER`, and has no notification
  object/ioctl lifecycle in the repository-wide source inventory. This does
  not establish runtime behavior or rule out a future implementation.
- **Unresolved:** signal provenance/reinjection, later-iovec `process_vm_*`
  partial-count semantics, FP/XSTATE, tracer-death cleanup, and the
  restart/cancellation/memory semantics of any proposed future Narf notifier.
  Cross-AS `process_vm_*`, RDTSC, and CPUID are verified gaps.
- **Unresolved:** provider-wide canonical ordering is above the API and must be specified by the POC/tool design.
- **Unresolved:** security against a hostile guest is not claimed by in-process providers and was not penetration-tested.
- **Deferred:** turning KVM, DBT, preload, SaBRe, or seccomp user notification into a conforming `Backend` would require new implementation; current helpers do not establish it.

## Runtime evidence decision

`/dev/kvm` and Rust 1.96.0 were present, but there was no prebuilt target cache. Building KVM/DBT/e9patch/SaBRe would pull in substantial native/vendor work and would test this Linux host, not Narf. The bounded investigation therefore stopped at exact-revision source analysis. No command result is represented as a backend pass.
