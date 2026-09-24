# Hermit/Linux source coverage ledger

Status: Audit-A correction coverage record. Source inspection only; no runtime command
was used to claim backend support.

## Revisions reconciled

| Source | Exact revision | Role |
| --- | --- | --- |
| `rrnewton/hermit` | `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2` | Hermit CLI, Detcore, trace, verifier, record/replay |
| Hermit's nested `rrnewton/reverie` | `ae1d1da78d2d89a1fa0454ee2282cf135e2286ed` | Actual Reverie code built by the pinned Hermit commit |
| Parent separate `rrnewton/reverie` | `123df7c4c0169006fbfe1f11a1553fe333eac937` | Contract's separately pinned current Reverie checkout |
| `hodgesds/narf` | `afe919924a5fd2c8e820ab3a0564b6d45438e6bc` | Cross-check of stock bulk-memory and CPUID/RDTSC prerequisites |
| Linux man-pages | `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7` | Linux ABI comparison text |

An ancestry/content check found `ae1d1da…` is seven commits after
`123df7c…`. The relevant trait/task/safeptrace/memory files are identical;
`tracer.rs` adds a startup-exit error fix. Evidence that describes Hermit's
actual runtime cites `ae1d1da…`; inventory facts about the separate current
checkout may cite `123df7c…` elsewhere in the project evidence.

## Paired precise and naive queries

Queries were executed with `rg` against pinned local trees; hit lists were read
into the files named in the coverage inventory rather than treated as facts.

| Question | Precise query | Naive control | Outcome / inverse control |
| --- | --- | --- | --- |
| Which backend does Hermit actually dispatch? | `Detcore<|TracerBuilder::<Detcore>|run_static_elf_with_tool_completion|run_host_with_preload` | `backend` | Found dedicated dispatch in `hermit-cli/src/lib.rs`; enum names alone excluded. |
| What is the backend-neutral boundary? | `trait (Backend|Tool|Guest|GlobalTool)|handle_(syscall|signal|timer|cpuid|rdtsc)` | `Tool Guest` | Found full lifecycle traits; inverse searched for a ptrace-only Detcore implementation and found none. |
| How are syscalls trapped? | `PTRACE_O_TRACESECCOMP|PTRACE_EVENT_SECCOMP|RET_TRACE|handle_seccomp` | `seccomp syscall` | Found RET_TRACE -> ptrace event -> Tool callback. Inverse searched USER_NOTIF in actual Hermit launch; only reusable process helpers, not dispatch. |
| Which ptrace options/events are required? | `PTRACE_(TRACEME|SETOPTIONS|O_TRACE|EVENT_|GETEVENTMSG|GETSIGINFO|GETREGSET|SETREGSET)` | `ptrace` | Option mask and event decoder found; inverse searched SEIZE/INTERRUPT/LISTEN/GET_SYSCALL_INFO in normal launch and found no use. |
| How are syscalls suppressed/injected? | `skip_seccomp_syscall|private_inject|untraced_syscall|orig_syscall_mut` | `inject syscall` | Found -1 suppression, SINGLESTEP, private-page call, PTRACE_SYSCALL exit. |
| How is guest data moved? | `process_vm_(readv|writev)|PTRACE_(PEEKDATA|POKEDATA)|MemoryAccess` | `read memory` | Found small ptrace-word and larger process-vm paths; Linux partial-transfer contract checked. |
| What does strict comparison preserve? | `BitwiseInfoV1|PARITY_STRIPPED_PREFIXES|PARITY_CANONICALIZATIONS|compare_io_buffers` | `log compare` | Found exact policy and fail-closed renderer; inverse searched numeric/path stripping in canonical mode and found it only in weaker Stripped mode. |
| What does record/replay capture? | `enum SyscallEvent|record_event|Replay diverged|record_or_replay_config` | `record replay` | Found typed event schema and explicit one-recording guarantee. Inverse searched for independent-recording equivalence; source explicitly rejects it. |
| Which inputs affect equivalence? | `virtualize_time|sequentialize_threads|sched_seed|rng_seed|mountinfo|network_trace|base_env` | `time random filesystem network` | Found config, metadata, environment and input boundaries; inverse searched active network-trace consumer and config says inert. |
| How are failures classified? | `enum Verdict|NoResultReason|InfrastructureError|first_divergent` | `divergence failure` | Found typed verdict and three semantic divergence classes; proposed cross-kernel classes remain separate. |
| Which default instruction events and enablement operations are required? | `Subscription::all\(\)|PR_SET_TSC|PR_TSC_SIGSEGV|ARCH_SET_CPUID|handle_(cpuid|rdtsc)_event` | `cpuid|rdtsc` | Found Detcore's fail-closed `all()`, Reverie's CPUID/RDTSC subscription bits and enablement operations, and Tool handlers. Inverse checked native execution without a delivered event; that is not a conforming subscribed path. |

## Raw paired-query receipts

All eleven precise/naive query pairs above were rerun from the registered
worktree on 2026-09-24. The raw command, stdout, stderr, and exit status for
each query are preserved under
`ignored/narf-hermit-poc-design-20260924/hermit/paired-queries-20260924/`.
`MANIFEST.txt` records `rg 15.2.0`, the Hermit and nested-Reverie revisions,
line/byte counts, and SHA-256 for every stdout receipt. Exit status 1 on the
four space-separated naive controls means the literal regular expression had
no match; those zero-hit receipts are retained rather than rewritten into a
positive query.

The receipt manifest records these precise/naive line counts respectively:
dispatch 146/821; boundary 607/0; syscall trap 19/2; ptrace 172/1689;
injection 31/2; memory 287/24; comparison 245/1; record/replay 87/0;
inputs 1146/0; failures 1178/0; instruction interception 169/3451. Search
hits are discovery evidence only; every promoted fact was checked in the
source ranges cited by the fact set and JSONL.

## Implementation coverage inventory

| Family | Files fully or materially read | Coverage conclusion |
| --- | --- | --- |
| Hermit dispatch | `hermit-cli/src/lib.rs`, `hermit-cli/src/bin/hermit/run.rs`, `hermit-cli/src/bin/hermit/backends.rs` | Actual `Detcore` dispatch distinguished from names/tests. |
| Reverie abstract lifecycle | `reverie/src/backend.rs`, `tool.rs`, `guest.rs`, `subscription.rs`, `lib.rs` | Backend/Tool/Guest/Subscription public surface inventoried. |
| Ptrace launch | `reverie-ptrace/src/lib.rs`, `backend.rs`, `tracer.rs` | TRACEME, initialization, option mask, seccomp filter, wait/run ownership read. |
| Ptrace events/injection | `reverie-ptrace/src/task.rs`, `injected_syscall.rs`, `timer.rs` | Stop dispatch, syscall callback, suppression, reinjection, signal/timer provenance read. |
| Safe ptrace and memory | `safeptrace/src/lib.rs`, `memory.rs`, `waitid.rs`, `notifier.rs` (targeted production sections) | Request wrappers, event decode, wait status, remote data path read. Large notifier test corpus excluded from facts. |
| Detcore event path | `detcore/src/lib.rs`, `tool_local.rs`, `tool_global.rs`, `scheduler.rs` (targeted symbols), `io_buffers.rs`, `random.rs`, `syscalls/time.rs`, `syscall_classification.rs` | Pre/post syscall, logical time, scheduling, buffer evidence, randomness and failure policy mapped. No claim that every syscall implementation was audited. |
| Comparison/reporting | `detcore/src/logdiff.rs`, `hermit-cli/src/bin/hermit/verify.rs`, `canonical_verdict.rs`, `docs/DIVERGENCE_CLASSES.md` | Canonicalization, observation receipt, typed failure/divergence mapped. |
| Record/replay | `event.rs`, `recorder.rs`, `replayer.rs`, `record.rs`, `replay.rs`, `metadata.rs`, `docs/RECORD_REPLAY_STATUS.md` | Current schema and configuration read; historical runtime report excluded as current evidence. |
| Runtime inputs | `detcore-model/src/config.rs`, `hermit-cli/src/bin/hermit/run.rs`, `metadata.rs`, `docs/DETERMINISM_ARGUMENT.md` | argv/env/filesystem/network/time/scheduling/hardware input classes mapped. |
| Default instruction interception | `detcore/src/lib.rs`, `reverie/src/subscription.rs`, `reverie-ptrace/src/tracer.rs`, `reverie-ptrace/src/task.rs` | Default fail-closed CPUID/RDTSC subscription, pre-exec TSC fault request, injected CPUID fault request, and Tool-event dependency read. |
| Narf prerequisite cross-check | `userspace/src/handlers/core.inc.rs`, `sys_prctl.rs`, `sys_arch_prctl.rs` at `afe9199…` | Distinct-AS process-vm rejection, accepted-but-inert TSC mode, and absent CPUID arch-prctl operation read directly. No Narf runtime claim made. |

## External document headings opened

The exact headings are recorded to prevent partial-read ambiguity.

- `ptrace(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; Death under ptrace;
  Stopped states; Signal-delivery-stop; Signal injection and suppression;
  Group-stop; PTRACE_EVENT stops; Syscall-stops; PTRACE_EVENT_SECCOMP stops
  (Linux 3.5–4.7 and since 4.8); PTRACE_SINGLESTEP stops; Informational and
  restarting commands; Attaching/detaching; execve under ptrace; Real parent;
  RETURN VALUE; ERRORS; STANDARDS; HISTORY; NOTES; access-mode checking; Yama;
  C library/kernel differences; BUGS; SEE ALSO.
- `seccomp(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; Filters; `/proc`
  interfaces; Audit logging; RETURN VALUE; ERRORS; STANDARDS; HISTORY; NOTES;
  Architecture support; Caveats; Seccomp-specific BPF details; EXAMPLES;
  Program source; SEE ALSO.
- `seccomp_unotify(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; Overview; IOCTL
  OPERATIONS; NOTIF_RECV; ID_VALID; NOTIF_SEND; NOTIF_ADDFD; NOTES;
  select/poll/epoll semantics; Design goals/CONTINUE; `/proc/tid/mem` caveats;
  blocking-call caveats; SA_RESTART; BUGS; EXAMPLES; Program source; SEE ALSO.
- `process_vm_readv(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; RETURN VALUE;
  ERRORS; STANDARDS; HISTORY; NOTES; EXAMPLES; SEE ALSO.
- `perf_event_open(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; Arguments;
  Reading results; MMAP layout; Overflow handling; rdpmc; ioctl calls; prctl;
  configuration files; RETURN VALUE; ERRORS; STANDARDS; HISTORY; NOTES; BUGS;
  EXAMPLES; SEE ALSO.
- `wait(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; wait/waitpid; waitid; RETURN
  VALUE; ERRORS; VERSIONS; C library/kernel differences; STANDARDS; HISTORY;
  NOTES; Linux notes; BUGS; EXAMPLES; Program source; SEE ALSO.
- `getrandom(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; RETURN VALUE; ERRORS;
  STANDARDS; HISTORY; NOTES; Maximum bytes; Signal interruption; BUGS; SEE ALSO.
- `signal(7)`: NAME; DESCRIPTION; dispositions; sending; waits; masks/pending;
  handler execution; standard signals; queue/delivery; numbering; realtime
  signals; syscall interruption/restart; stop-signal interruption; STANDARDS;
  NOTES; BUGS; SEE ALSO.
- `execve(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; process attributes;
  interpreters; argument/environment limits; RETURN VALUE; ERRORS; VERSIONS;
  scripts; STANDARDS; HISTORY; NOTES; EAGAIN; EXAMPLES; SEE ALSO.
- `clone(2)`: NAME; LIBRARY; SYNOPSIS; DESCRIPTION; wrapper; clone3;
  equivalence; termination signal; set_tid; flags; RETURN VALUE; ERRORS;
  VERSIONS; C library/kernel differences; architecture notes; STANDARDS;
  HISTORY; NOTES; BUGS; EXAMPLES; Program source; SEE ALSO.

## Project document headings opened

- `docs/Developers/Architecture.md`: Hermit System Architecture; global/local
  Detcore components.
- `docs/Users.md`: Hermit Users Guide; Further reading.
- `docs/DIVERGENCE_CLASSES.md`: discriminator; three classes (pure-clock, true
  guest, pure-observation); visibility history; provenance.
- `docs/TIMEOUT_LADDER.md`: rungs; test policy; enclosing budgets; termination;
  class line; backend qualification; invariant; known gap.
- `docs/DETERMINISM_ARGUMENT.md`: claim scope; syscall semantics; entropy;
  mitigation; resource checkout; informal proof; edge cases; test evidence;
  template; outcome.
- `docs/RECORD_REPLAY_STATUS.md`: scope; method; environment; basic programs;
  nondeterminism/runtimes; OSS applications; regression suite; blockers;
  recommended checks.
- `README.md`: requirements; install; quick start; execution backends;
  compatibility; workflows; DAP; chaos; performance; architecture;
  troubleshooting; contribution; documentation.

Protocol/control documents were also read completely but are not technical
evidence: `research-contract.md` (Decision; In scope; Out of scope; Source
state; Required primary sources/comparisons; Required inventories/matrices;
Review/publication boundary; Claim labels), `evidence/README.md` (Evidence
workspace), deep-research `SKILL.md` (query classification; mandatory design
profile; reconnaissance; orchestration; synthesis; trust gate; cleanup; tools;
reporting/resources), `design_document_audit.md` (required sequence; roles; stop
rule), `subagent_template.md` (work loop; communication; tool selection; budget;
output; checklist; pitfalls), and `finding_schema.md` (schema; fields; evidence
types; helper; storage). Repository `AGENTS.md` was treated as operating policy,
not as a primary source for product capability.

## Document classes

| Class required by protocol | Searched? | Result / declared gap |
| --- | --- | --- |
| Current implementation | Yes | Primary source for all material Hermit/Reverie claims. |
| Executable/runtime evidence | No new run | Explicit gap; no Hermit-on-Narf backend exists to execute. Historical logs/docs are not promoted to current evidence. |
| Versioned specifications/interfaces | Yes | Linux man-pages exact commit plus public Rust traits. |
| Tests | Searched, selectively read | Used to derive acceptance primitives and negative controls only; existence is not support. |
| Landed diffs/history | Revision ancestry and commit diff only | Used to reconcile Reverie pins. No historical diff used to prove runtime behavior. |
| Design documents | Yes | Architecture and determinism documents read as context, checked against source. |
| Launch/admission threads | Not applicable/public evidence absent | Declared gap; no project launch thread used. |
| Planning/leadership posts | Not searched | Out of this fact lane and not primary evidence. |
| Retrospectives | Project divergence/status documents read | Historical measurements excluded from current runtime claims. |
| Deprecation notices | Searched in ptrace/man-pages and source comments | No separate Hermit deprecation artifact found; ptrace API caveats retained. |

## Seen and excluded

| Source | Disposition | Reason |
| --- | --- | --- |
| `hermit/AGENTS.md` | Context only | Repository policy/terminology, not proof of implementation. |
| `reverie/BACKENDS.md` | Excluded from material facts in this lane | Another agent inventories all backends; source is authoritative here. |
| `docs/RECORD_REPLAY_STATUS.md` | Read, runtime tables excluded | Measurements are at older Hermit revision/environment and cannot prove current behavior. |
| `docs/DIVERGENCE_CLASSES.md` | Used only for taxonomy | Its historical examples are not generalized into current runtime support. |
| Unit/integration test names and comments | Excluded as support evidence | Used only to discover code paths and specify future falsifiable tests. |
| CLI backend enum | Used only as an inventory lead | A spelling is not proof that the path executes Detcore. |
| `safeptrace` tests/notifier stress corpus | Mostly excluded | Test presence does not prove Narf semantics; production methods/events were read directly. |
| `reverie-process` USER_NOTIF helpers | Labeled software helper | No actual Hermit dispatch uses them at this revision. |
| Narf facts received from peer lane | Preserved with cross-agent qualifier | Orchestrator must reproduce them from Narf primary source before Audit A locks. |

The three Narf files used for the Audit-A stock-prerequisite corrections were
subsequently read directly at the pinned revision, so those specific
process-vm and instruction-faulting facts no longer rely on the peer qualifier.

## Inverse checks and gaps

1. If current Narf ptrace were sufficient, its source/runtime would show
   `PTRACE_EVENT_SECCOMP`, child/exec/exit event status encoding,
   `GETEVENTMSG`, `GETSIGINFO`, register/memory semantics and wait behavior.
   The Narf lane reports the opposite for the first four; the remaining surface
   is an explicit unresolved matrix.
2. If Hermit normal launch required SEIZE/INTERRUPT/LISTEN, those symbols would
   appear in `TracerBuilder`/task production paths. They do not; TRACEME is the
   selected root path. `safeptrace` helper availability is not a requirement.
3. If USER_NOTIF were the actual Hermit syscall path, dispatch would create a
   listener and call NOTIF_RECV/SEND. It instead installs RET_TRACE and requires
   TRACESECCOMP; reusable notifier definitions are above the active path.
4. If strict comparison erased numbers/paths/timestamps, canonical policy would
   include the unsafe normalization token. It instead permits wall-prefix
   stripping and host-address ordinals only; wholesale stripping is a distinct,
   weaker mode.
5. If record/replay verification proved independent executions equal, its config
   would virtualize time or state that claim. Source explicitly says the
   opposite.
6. If I/O return values captured all behavior, buffer hashing would be
   unnecessary. Current parity explicitly requires output-buffer content.
7. If stock bulk `MemoryAccess` were compatible with current Narf, the Narf
   target path would accept a distinct tracer address space. It explicitly
   returns `EPERM`; the same-AS page-boundary partial-transfer details remain a
   separate unresolved semantic question.
8. If the default Detcore instruction subscriptions were deliverable, Narf
   would make `PR_TSC_SIGSEGV` fault `rdtsc` and implement
   `ARCH_SET_CPUID(0)`. Source instead accepts the former without disabling the
   instruction and rejects the latter through the catch-all `EINVAL` arm.
9. Missing evidence: no Narf runtime; no Narf/Hermit boot; no verified Narf
   seccomp/perf timer surface; no cross-provider observation-coverage receipt;
   no proof that all current Detcore syscalls share identical semantics on
   Narf. These remain **Unresolved**, never silently negative or positive.

## Structured-finding schema profile

`hermit-findings.jsonl` uses the explicitly versioned repository-local profile
`hermit-evidence/v1`. It preserves every canonical field and evidence type but
names records `H-FN` so they remain collision-free when the Hermit, Reverie,
and Narf lanes are concatenated. This is the sole ID-grammar deviation from the
canonical `F-N` schema. Every `file_ref.snippet` is a literal contiguous byte
sequence from the pinned source range; ellipsis-based paraphrases are forbidden.

## Denominator discipline

No new percentage is asserted by this lane. Historical percentages found in
comments/status reports (for example I/O-buffer coverage measurements) are not
used to quantify present cross-kernel coverage. Any final percentage must name
its denominator: subscribed syscall numbers, executed syscall instances,
compared structured records, acceptance cells, or backend/provider cells.
