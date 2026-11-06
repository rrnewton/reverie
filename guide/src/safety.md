# The safety contract and refusals

Reverie runs arbitrary tools against arbitrary guests. The contract
that keeps that honest has three clauses, and this chapter states them
as obligations on *us* (the framework), not as advice to you.

## 1. Same semantics, or a named refusal

A tool's observable behaviour must not depend on which backend ran it.
Where a backend cannot provide the semantics — a lifecycle case, a
`Guest` operation, a binary shape — it refuses, by name, preferably
before the guest starts. Silent approximation (a statistic that reads
zero, an event quietly unobserved, a handler skipped) is a contract
violation, and the conformance suite exists to catch it
([Testing](testing.md)).

## 2. Fail closed at every boundary

* **Trapping:** a syscall no hook covered still reaches the
  dispatcher or fails; it never executes unobserved by accident.
  Unsubscribed syscalls execute only because the tool's subscription
  *declared* them unobserved.
* **Patching:** a site with no hook runs the fallback continuation;
  only a refused site (or an unavailable continuation) fails with
  `EOPNOTSUPP` — never a best-effort patch and never arbitrary Rust
  in signal context.
* **Activation:** a mode with an unmonitored window (preload) must be
  *chosen* with the window declared. The no-silent-downgrade rule is
  enforced mechanically by admission
  ([Mechanisms](mechanisms.md)): a windowed cell is eligible only
  when the tool declared that window.
* **Statistics:** unsupported measurements are absent, not zero.

## 3. The guest is not trusted; neither is the tool

Guest pointers are claims, validated at use; guest code is data until
a mechanism admits it. Symmetrically, the framework treats tool
handlers as fallible: handler errors fail the run rather than letting
a guest continue under a broken tool, and in-guest runtimes confine
their own syscalls to audited gate ranges so a tool bug cannot become
an unobserved kernel interaction.

## Lifecycle: where contracts go to be tested

Threads (`clone`), signals (including who owns `SIGTRAP`/`SIGSEGV`
against guest handlers), `fork`, `exec` and successor images, `vfork`,
and mapping churn (`dlopen`, JIT code) are where per-backend semantics
diverge first. The current honest state, in brief: the ptrace backend
follows the full lifecycle (with *Partial* signal fidelity — named
held-signal holes that count as part of the reference semantics);
in-guest paths cover documented subsets
and refuse outside them (guest-installed callable signal handlers are
the canonical example); KVM's personality is narrower still. Any claim
stronger than the per-backend tables in the repository's
`BACKENDS.md` should be treated as a guide bug.

## The refusal names (`R-*`)

Every refusal is a typed value surfaced *before* the guest runs (or, for
mid-run discoveries, at the fault with the guest stopped). "Unsupported"
without a name is a bug. The taxonomy is architecture §3.4, approved
2026-11-05, and it is public API: `reverie::selection::RefusalCode`,
carried by `AdmissionRefusal` — `code` (the machine-readable name),
`detail` (one human line, never the only signal), `trace` (which cells
each filter stage removed and why), and `terminal` (true when the
failure was discovered after irreversible installation, where the host
must stop rather than retry another backend). It prints as
`REFUSED: R-…: <detail>`. An admission refusal is distinct from a guest
errno and from per-operation unsupported results; the older surfaces
still exist underneath it (a refused patch site still fails with
`EOPNOTSUPP` *to the guest*, unprovided facts are still `NotProvided`)
— those are mechanisms, this is the run-level verdict.

| Name | Trigger | Remedy |
|---|---|---|
| `R-ARCH` | Not Linux x86-64, or kernel floor unmet (P10) | Run on a supported host/kernel; no requirement relaxes this |
| `R-PTRACE` | ptrace required but denied, or guest already traced (P1, P2) | Free the guest from its tracer, grant ptrace, or pick a no-ptrace cell when one is built |
| `R-WINDOW` | Only windowed activation available; tool requires zero window | Declare the window in `ToolRequirements`, or use a zero-window activation |
| `R-CODE` | Code cannot be censused/gated for the mechanism: JIT, interior entries, unpatchable site with no fallback | Trap/gate/translation cell, or exclude the code shape from the guest |
| `R-SIGNAL` | Required signal ownership unavailable (`SIGTRAP`/`SIGSYS`/`SIGSEGV` conflict) | Resolve the ownership conflict; the guest handler and the framework cannot both own it |
| `R-WX` | Mapping policy forbids the arena/gate shape (strict W^X) | Out-of-process cell (V1), or a W^X-clean runtime shape |
| `R-INSTR` | A required nondeterministic-instruction class cannot be mediated in any admitted cell | Drop the instruction requirement, or provide the mediation prerequisite (e.g. CPUID faulting) |
| `R-EXEC` | Successor image cannot be re-armed under the retained plan | Cell with re-arming, or a guest that does not exec |
| `R-LIFECYCLE` | Required thread/fork/signal behaviour unproven in every admitted cell | Narrow the lifecycle requirement to a proven envelope |
| `R-DENSE` | Dense (every-branch) instrumentation required; no translation cell implemented | Event-level instrumentation until a B6 cell exists |
| `R-UNBUILT` | Only valid-but-unbuilt design cells satisfy | Wait for the cell, or relax to an implemented one |
| `R-ENV-UNKNOWN` | A load-bearing environment fact could not be proved or disproved | Make the probe decidable (permissions, custody, host support); unknown never counts as proved |

Refusals carry their filter trace — which cells each stage removed and
why — so "why won't it run?" is answered by the log, not by reading
source. Invalid combinations have names too (I1–I8 in architecture
§2.3): preload can never serve a zero-window tool (I1); patching alone
cannot cover arbitrary/JIT code (I2); no combination's *correctness*
may require SUD, CPUID faulting, or `PR_SET_TSC` (I6, as amended: such
support may be a cell prerequisite, its absence then yields a named
refusal above); and so on.
>
### The lifecycle matrix

Coverage below is generated from the three `Guest` implementations
(which lifecycle hooks each backend actually drives) and checked
against the conformance corpus's lifecycle programs (shell
fork/exec, exit codes); "delivered" means the corresponding `Tool`
hook runs on that backend today. Anything not delivered is refused
or outside the cell's stated personality — never silently skipped.

| Event | ptrace | in-guest | KVM |
|---|---|---|---|
| Thread birth (`handle_thread_start`) | delivered | delivered (per-process runtime; new threads initialize continuation state before their first trap) | delivered (backend-created vCPU threads) |
| Fork / clone children followed | delivered — the reference | a supported COW fork inherits the live image and stack and rebinds the child owner; broader topologies are not claimed | emulated inside the VM personality (LinuxEmu has its own fork/vfork task model); no host process is forked |
| Exec successor (`handle_exec_commit` / `handle_post_exec`) | delivered, `handle_post_exec` per successor image | re-armed by the preload constructor when the preload survives the exec; `handle_post_exec` delivered | `handle_exec_commit` delivered (the one backend that can refuse an exec before the point of no return); `handle_post_exec` delivered |
| Signal interception & rewriting | delivered, with the named held-signal holes of §2.2 as reference semantics | shared dispatcher over SIGSYS/SIGSEGV with an alternate-stack runtime; arbitrary guest signal-handler support is refused, not emulated | guest signals are personality-emulated; host signal 64 and `SIGSTKFLT` are reserved by the backend and must not be used by embedding code |
| Mapping churn after launch | observed (mappings are syscalls and page state) | refused outside the admitted census: new executable code needs its own admission | managed as VM memory by the backend/LinuxEmu, not guest-planned mappings |
| Thread/process exit (`on_exit_thread` / `on_exit_process`) | delivered | delivered | delivered |
| Timers | delivered (time and precise RCB schedules; timers armed before the launch exec are retired by it — re-arm in `handle_post_exec`) | delivered through the same `Guest` surface where the runtime admits the guest shape | delivered; the preemption timer's `SIGSTKFLT` is backend-owned |

Two asymmetries in that table bite in practice. Only KVM has an
exec *commit* point it can refuse before the old image is gone —
elsewhere `handle_post_exec` is your first word in the new image.
And only ptrace delivers the full signal surface: in-guest signal
handling is a dispatcher with stated limits, and KVM reserves two
host signals for itself, so a tool that rewrites arbitrary signals
today has exactly one backend.
