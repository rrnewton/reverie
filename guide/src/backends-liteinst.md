# Backend: LiteInst in-guest (`reverie-liteinst`)

The patching in-guest backend. Cell **V5-preload-patch** in
`reverie::selection::implemented_cells`: preload activation with a
*declared* pre-init window, in-place patching as the mechanism,
tool in the guest. `LiteinstBackend` implements the generic
`Backend` trait within the envelope below; outside it, the answer
is a named refusal, not a slower approximation. With site
patching switched off, the same runtime is the
[in-guest-trap](backends-inguest-trap.md) backend.

## Mechanism

* **Hooking:** the first execution of a syscall site reaches the
  shared seccomp/`SIGSYS` trap. The dispatcher installs a
  LiteInst hook at that site and redirects the saved signal
  context to it, so the first and every later call enter the same
  normal-context tool callback — patched calls pay no trap.
* **Trapping:** the shared `reverie-inguest` trap is the
  completeness backstop for first use and for sites that stay
  unpatched. A generic-tool site that cannot be patched and has
  no fallback continuation fails with `EOPNOTSUPP`; arbitrary
  Rust never runs in signal context as a substitute.
* **Placement:** a tool DSO hosts process/thread state inside the
  guest. Global state stays in the launcher's `RpcServer`,
  reached through the preload coordinator RPC client. Your tool
  must therefore be compiled into a preload — at the example
  layer, a missing preload is a by-name refusal
  (`RunError::Refused`), not a silent fallback to ptrace.

## Lifecycle coverage

The admitted envelope is deliberately narrow, and admission (not
the guest's luck) enforces it: dynamically linked, non-`AT_SECURE`
guests that a preload can enter; **single-thread** — thread
creation refuses by name; **exec successors refuse** in this cell;
fork is followed and vfork is declared-weaker; signals are
declared-weaker (the shared dispatcher owns the trap signals, and
guest-installed callable signal handlers are refused, not
emulated). Signal ownership is held at handoff
(`signal_ownership_at_handoff`). CPUID/RDTSC mediation require
the P5/P6 environment prerequisites; RDRAND/RDSEED and XGETBV are
absent. Hook delivery within the envelope is in the
[lifecycle matrix](safety.md#the-lifecycle-matrix).

## Refusals and boundaries

* `R-WINDOW` — a tool that requires a zero unmonitored window can
  never use this (or any preload) cell; the window must be
  declared in `ToolRequirements`.
* `R-CODE` — code that cannot be censused or gated for patching
  (JIT shapes, interior entries, an unpatchable site with no
  fallback).
* `R-EXEC` / `R-LIFECYCLE` — successor-image re-arming, or
  thread/lifecycle behaviour beyond the envelope above.
* Static or `AT_SECURE` guests never see the preload and are
  outside the envelope, as is anything that happens before the
  preload constructor (the declared window).

## Statistics

`run_with_stats` yields a `LiteinstBackendStatsSnapshot`
(`BACKEND_NAME = "liteinst"`, `mode() == InGuest`), aggregated
from per-process guest reports:

| Accessor | Reports |
|---|---|
| `process_reports()` | process-local reports aggregated — totals cover only processes that reported |
| `patch_shapes()` | candidate/patched RIP counts and shape distribution, deduplicated by process, exec generation, and RIP |
| `patch_decisions()` | per-site outcome counts: `DirectPun`, `Relocated`, `StraddlerFallback`, `OtherFallback` |
| `dispatch_paths()` | how entries actually arrived, per `LiteinstDispatchPath` |
| `fork_child_entries()` | entries a fork child re-reported as its own first event (kept here, subtracted in the shared record so each physical entry counts once) |

The shared-record projection counts every received `SIGSYS` as a
signal trap (including the one that installs a site's hook),
patched hook entries as direct calls, and fallback refusals as
refusals. A process killed before it could report is *missing*
from the totals — `process_reports()` is how you know.

## Run it

Through the examples' shared launcher this is `--backend
liteinst` (see [your first tool](first-tool.md#the-launcher-pick-a-backend-get-state-back));
the judged corpus on this backend alone is:

```console
$ reverie-examples/e2e/run.sh --backends liteinst
```

On the [scorecard](testing.md) it matches native on **14/23**
programs, with mismatches clustered exactly at the boundaries
above (successor `execve`, process-tree programs, late-start
alignment on large runtimes). When those boundaries are yours,
the refusal — or ptrace — is the product working.
