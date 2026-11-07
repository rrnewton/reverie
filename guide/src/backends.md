# Backends

A backend is a complete implementation of process supervision and
event interception that can host an **arbitrary** `T: Tool` — the tool
type is always a generic parameter, never hard-coded. This chapter
introduces the backends a tool author chooses among. The backend
*author's* contract (what you must build to add one) is
[Writing a backend](writing-a-backend.md); the deep mechanism
comparison is the repository's `BACKENDS.md`, which this chapter
summarizes and must not contradict.

## The backends at a glance

| Backend | Mechanism | Generic `Backend`? | Tool runs… | Status |
|---|---|---|---|---|
| `reverie-ptrace` | seccomp `TRACE` filters + ptrace stops | yes — the reference | in the tracer | full lifecycle following; **signal fidelity Partial with named holes** (held-signal windows — and "same semantics" includes those holes until fixed in the reference first); slowest per event |
| `reverie-liteinst` | shared seccomp `SIGSYS` trap; first use installs a LiteInst patch at the site | yes, with boundaries | in the guest (tool DSO) | dynamically linked x86-64; documented lifecycle limits |
| in-guest-trap | LiteInst runtime with site patching off: every syscall traps | via the same runtime | in the guest | the patching backends' semantic reference |
| `reverie-kvm` (+ `-core`) | VM exits from a KVM guest | specialized runner, not the generic trait | in the host, per-thread state in `KvmGuest` | static-ELF personality, growing. **`reverie-linuxemu` is KVM-internal** — its userspace Linux emulation is a building block of this backend, not a backend or Tool-contract surface of its own, and it gets no column in the conformance matrix |
| e9patch, SaBRe, DynamoRIO | AOT rewrite, load-time rewrite, DBT | e9patch has a generic (ptrace-hosted) mode | varies | external repositories own these |

In architecture vocabulary a **cell** is one activation (A) × one
mechanism (B) × one placement (D) combination — and a single backend
can host more than one cell (`LiteinstBackend` runs both the B3
patching cell and, with patching off, the B1 in-guest-trap cell):
ptrace is A1×B7×out, `InGuestTrap` is A2×B1×in today, and so on
([Mechanisms](mechanisms.md) has the full axis list). Note also that
Reverie's LiteInst binding still pins an October LiteInst revision
(1,971 commits behind `liteinst` 0.2.0): "LiteInst can do X" and "a
Reverie tool gets X" are different claims until that port lands, and
conformance results must name the pin they ran on.

Three independent questions hide inside "which backend":

1. **Hooking** — what makes an event leave normal execution?
2. **Trapping** — what catches an event that was *not* hooked, and
   does it fail closed?
3. **RPC placement** — where does global state live, and how does a
   handler reach it?

[Interception mechanisms](mechanisms.md) takes those one at a time.
For choosing today: start on **ptrace** (the reference semantics,
every lifecycle case), keep it as your correctness oracle, and move
hot tools to in-guest execution when measurement says to.

## Boundaries are part of the API

Each backend publishes what it refuses. Examples from the current
tree: the generic LiteInst backend rejects guest-installed callable
signal handlers and clone/fork injection, within a documented
one-process/one-thread envelope for its generic path; the KVM runner
refuses what its static-ELF personality cannot express; in-guest
direct e9patch mode is single-process/single-thread. A tool that
needs more gets a named refusal at launch — that is the contract
working, not failing.

Backend statistics and captured output are also contractual:
`run_with_stats` returns a typed snapshot from the backend's real
collector (unsupported measurements are never a successful zero), and
`run_with_output` returns guest stdout/stderr bytes with the status
and final state.

## Per-backend pages

Each first-party backend has a reference page in this book —
mechanism (hooking, trapping, placement), lifecycle coverage
from its admission envelope, its refusals by name, its typed
statistics snapshot field by field, and the exact example
commands. They summarize the same sources of truth as this
chapter (`BACKENDS.md`, `reverie::selection::implemented_cells`,
and the [conformance scorecard](testing.md)) and must not
contradict them:

* [ptrace](backends-ptrace.md) — the reference, V1.
* [LiteInst in-guest](backends-liteinst.md) — patching, V5.
* [in-guest-trap](backends-inguest-trap.md) — the same runtime
  with patching off, V4, and the patchers' semantic reference.
* [KVM](backends-kvm.md) — the VM substrate, V11.

The three external backends are covered as a group below; their
owning repositories hold their reference documentation.
## External backends

Three backends live outside this workspace, in extension
repositories that own their native source custody, builds, and
qualification. `docs/BACKEND_SOURCES.md` in this repository is the
ownership record; the summary below is why you would reach for each
and what contract it has *here*. Nothing in a core build qualifies
an extension: source archives, missing or dirty checkouts, and
relocated historical pins refuse in the extension's own producer,
and the old `third-party/{dynamorio,sabre,e9patch}` gitlinks in
this tree are retired — use each extension's normal checkout.

| Backend | Owning repository | Generic `Backend`? | Mechanism in one line |
|---|---|---|---|
| DynamoRIO | [hermit-dynamorio](https://github.com/rrnewton/hermit-dynamorio) (complete patched fork under `third-party/dynamorio`; `reverie-dynamorio` owns its SDK producer) | No — real `Tool`/`Guest` execution, not yet the common launch contract | Translated basic blocks in a code cache; syscall callbacks at the translated boundary; CPUID/RDTSC replaced in-cache |
| SaBRe | [hermit-3rdparty-patching](https://github.com/rrnewton/hermit-3rdparty-patching) (vendored under `experimental/reverie-sabre/vendor/`) | No — adapter with local and remote modes | Load-time `.text` rewrite: five-byte jumps to a plugin handler, `SIGILL` fallback where a jump cannot fit |
| e9patch | [hermit-3rdparty-patching](https://github.com/rrnewton/hermit-3rdparty-patching) (`reverie-e9patch/vendor/e9patch`) | **Yes** — the generic mode implements the trait | Ahead-of-time rewrite of every recovered syscall site in the root ELF, partial coverage refused at preparation |

How they differ on the three questions of this chapter:

* **DynamoRIO** hooks by translation, so there is no *missed native
  site* inside translated code — and no shared seccomp/SIGSYS
  completeness trap either; its boundary is the code cache. The
  tool runs in the DynamoRIO client; coordinated launches share one
  RPC server for global state, with an in-process singleton as the
  uncoordinated fallback. Native custody is the strictest of the
  three: the producer requires exact clean Git roots and all pinned
  recursive children, and measures source bytes, modes, and
  symlinks itself.
* **SaBRe** hooks by rewriting mapped executable text at load. Its
  honesty boundary is the one to read carefully: an unrewritable
  known site falls back to a reserved instruction and a `SIGILL`
  handler, but the current host launch installs no independent
  seccomp completeness filter, so a site SaBRe never found is not
  *proved* fail-closed by this integration. The plugin runs the
  tool in guest context; RPC placement is a mode choice (remote
  coordinator or local adapter), not part of the rewriter.
* **e9patch** comes in two modes that must not be confused. The
  *generic* mode implements `Backend` by staying ptrace-hosted:
  rewritten sites emit a validated `SIGTRAP` to the ptracer, which
  keeps loader syscalls, lifecycle, signals, timers, and full
  `Guest` semantics — so rewritten sites still pay a ptrace stop.
  The *direct opt-in* mode calls the shared dispatcher in guest
  context behind the preload seccomp/SIGSYS completeness trap, with
  a stated boundary (no static/`AT_SECURE` guests, no early loader
  calls, no exec) and currently single-process/single-thread
  lifecycle coverage — it does not replace the generic mode yet.

License notices travel with the extensions (DynamoRIO:
BSD-3-Clause with LGPL/BSD-4 components; SaBRe and e9patch:
GPL/LGPL families with documented exceptions — the table in
`docs/BACKEND_SOURCES.md` is authoritative). For mechanism-level
detail with source-pinned references, the repository's
`BACKENDS.md` is the backend author's reference; this section is
the tool author's map to it.
