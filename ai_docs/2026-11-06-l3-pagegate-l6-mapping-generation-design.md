[hermit2, musepage, tbh-1005, devbig030, role=impl]
# L3 page-fault gating (B4/V6) + B5 scan, on the L6 mapping-generation protocol (2026-11-06)

Author: musepage (Muse lane, devbig030), for museli-lt to route for
review. Status: **DESIGN — no code.** No implementation starts from
this note until museli-lt routes a review of it and the design is
approved.

Source: coord7 09:00 message assigning L3 of
`ai_docs/plans/2026-11-04-liteinst-reverie-full-court-press-plan.md`
(page-fault gating B4/V6 as a Reverie in-guest interception mode,
plus ahead-of-time scan B5), unblocked by first designing its L6
mapping-generation/revocation blocker. Slot:
`worktrees/slots/musepage-20261106` (Reverie checkout, branch
`musepage/pagegate-design-20261106`, base `main` `778a446c`).
Architecture of record:
`ai_docs/plans/2026-11-04-liteinst-reverie-architecture.md`
(V6 row, L6 dependency, 1.5x ruling in §7.1). Spike landed by
https://github.com/rrnewton/reverie/pull/1290 (grouped admission:
6 groups for 575 pages on `ls`, 1.57 µs/group fault+handler+grant,
5/5 witness tests). In-guest divergence audit:
`ai_docs/plans/2026-11-05-inguest-backend-convergence-audit.md`.

**One-sentence proposal.** Make the certified mapping group — one
immutable executable extent under one mapping generation — the
single admission unit for both patched sites and gated pages, mint
and revoke generations synchronously in the mapping-syscall path
under one L6 owner, admit a group on its first fetch by checking a
B5 certificate computed ahead of time (never by scanning on the
fault path), and expose the result as exactly one new
`REVERIE_INTERCEPT` value (`page-gate`) selecting the existing
`Mechanism::PageFaultGate` cell — registered as implemented only
when a zero-window activation for it actually exists.

## 0. Decision requested, non-goals

**Decision requested from reviewers (via museli-lt):** approve or
amend (1) the L6 revocation-before-resume ordering and single-owner
rule in §2, (2) the one-variant `PageGate` extension to musearch's
W5 enum in §3 (stacked after W5 slice 1, never a fork of it),
(3) the B5 certificate identity policy in §4 (stat-custody fast
path vs content hash), and (4) the slice order in §6 (L6 registry
first, gate second, selectable cell last). Three coordination
questions for museemu are listed in §7 as questions for museli-lt
to relay; this note does not claim museemu's agreement.

**Non-goals.** No Hermit/Detcore behaviour (Reverie-generic only,
per the slot `AGENTS.md`). No aarch64 gate claim: V6 here is
Linux x86-64, kernel floor 6.x; on aarch64 the strategy must
refuse by name until W3/W4 land a gate. No subsidy from SUD:
page-gate correctness never requires SUD (architecture I6);
SUD stays an x86_64-only optional acceleration inside admitted
pages. No dense instrumentation (B6/V10) and no arbitrary-JIT
success claim: uncertified dynamic code refuses `R-CODE` outside
V10. No silent fallback: anything outside the envelope refuses by
name (§2.6), never runs uninstrumented.

## 1. Facts this design stands on (verified in the slot tree)

Labels match the L0 architecture: **Verified-current** = read in
the pinned slot code; **Partial** = code exists with a narrower
envelope; **UNMEASURED** = no same-tree number exists.

1. **V6 is Buildable, not selectable.** L0 §2.1: V6 =
   A1/A3 × B4 × in — page-gate admission then B3/B1 inside
   admitted pages; zero window for gated mappings, ungated/new
   code faults closed or refuses. §2.2 Dyn cell: the answer for
   *certified* code; arbitrary JIT still `R`. Today only two
   in-guest modes are selectable — `Liteinst` and `InGuestTrap`
   via `REVERIE_LITEINST_SITE_PATCHING` (`reverie-liteinst/src/runtime.rs:374`,
   `reverie-examples/src/shared_host.rs:31-40`). B4 exists only as
   three standalone liteinst shapes (L0 §1.2:
   `liteinst-inprocess`, `tools/page-entry`, `liteinst-launch`),
   none bound to Reverie. L3's job is that binding plus the B5
   front end (L0 §1.2, gap G-6 → L3).
2. **L6 must land first.** Full-court plan M2: "Page-gate (L3, on
   L6's mapping-generation/revocation protocols, which land
   first)". Astra amendment D: "L6 mapping-generation/signal/
   ownership protocols precede L3 publication integration." L3
   has been hanging since 11-05 on exactly this (coord7 source).
3. **The grouped-admission model is witnessed; the backend is
   not.** Slot files `reverie-liteinst/src/bin/grouped_admission_spike.rs`
   (protocol model: "not a backend mode and wires into no
   runtime") and `reverie-liteinst/tests/grouped_admission.rs`
   (5 subprocess witnesses: group admit, interior refuse, stale
   refuse, re-admit, racing admits-once). Landed in Reverie
   https://github.com/rrnewton/reverie/pull/1290 (merged
   2026-11-06T13:05:25Z; spike commits `acb46c8c`, `d6d666bb`).
   Spike commit measurements (AMD EPYC 9D85): initial-image RX
   census `ls` 6 groups / 575 pages, `true` 3/418, `bash` 4/649;
   cold sha256 over exact RX extents of the `ls` closure median
   1.63 ms (n=10); stat-identity custody 7 µs; fault+handler+grant
   median 1.57 µs per group admission **excluding sealing and
   sync-core**. The spike hard-codes `PAGE = 4096` and one
   two-page group; both are model limits, not shippable shape
   (64K pages are a hard requirement, §5).
4. **Per-page scanning is refuted by measurement.** L0 §7.1
   (li2adm cost model, 2026-11-02, medians over 10 runs): `ls`
   native 4.17 ms / page-gated 19,382 ms / DynamoRIO-null 26.3 ms
   (737x DR-null); `true` 3.73 / 6,770 / 20.1 ms (337x). Parity
   budget ≈4.1 µs/admission over 5,393 pages vs ≈8.33 µs for one
   NX fault+grant+close with syncs *before* scanning; one cold
   full-page scan ≈350 µs; an instrumented `ls` spent 11.07 s of
   20.67 s in 379,086 fresh `/proc` maps acquisitions. At the
   1.5x bar the per-admission budget if all 5,393 admissions
   remain is 6.54 µs — already below the 8.33 µs NX cycle, so
   "cached scans alone cannot meet it; grouped certified
   admission … is the plausible, unestablished route" (L0 §6.3).
   L0 §7.1 consequence for L3, binding here: no cold scan or maps
   re-read on the fault path; facts computed ahead of time (B5)
   or memoized per mapping generation with real proof/ownership,
   never a cached boolean; every spike reports per-admission
   cost against the primitives plus whole-program DR-null ratio.
5. **Patch-site revocation exists; group revocation does not.**
   `observe_mapping_generation` (`reverie-liteinst/src/runtime.rs:1601`,
   `TODO-HUMAN-REVIEW(PR-127)`) marks sites `SITE_STALE` and sets
   `adapter_mapping_replaced` *after* a successful mapping
   syscall (`event.result < 0` returns early), on three dispatch
   paths (`runtime.rs:3013,3264,3470`; the compat path forwards
   first, then observes, `:3463-3470`). Covered: `mmap`,
   `munmap`, `mremap` (old + new range), `mprotect` **only**
   when `PROT_READ` is removed, and every `pkey_mprotect`
   (`:1605-1631`). Not covered (design must close, §2): an
   `mprotect` that keeps `PROT_READ` but changes `PROT_EXEC`
   is not observed today (no gate exists yet; census inputs
   stay admitted); no generation counter
   exists for groups (only per-site flags + one bool per arena);
   no re-sealing of any NX extent happens, because nothing is
   gated. Stale-site survival is byte-proved, not assumed
   (`stale_jump_survives`, `:2217`; `SITE_STALE` keeps its
   footprint reservation, `:2164-2170`) — L6 extends that
   discipline, it does not replace it.
6. **Selection + probes already exist in-tree.** Reverie
   https://github.com/rrnewton/reverie/pull/1290 also landed
   `reverie/src/selection.rs` (requirements, implemented-cell
   admission, typed `R-*` refusals, retained plans) and
   `reverie/src/probes.rs` (P1–P10 with pidfd trial custody;
   Unknown fails closed). `Mechanism::PageFaultGate` (B4) and
   `DesignCell::V6` exist (`selection.rs:579-580,763-764`), but
   `implemented_cells()` registers only V1-ptrace,
   V4-preload-trap, V5-preload-patch, V11-kvm (`:1115-1280`) —
   no V6 cell is selectable, correctly (Astra: a Buildable row
   is an obligation, not a cell). musearch's W5 branch
   `musearch/w5-intercept-20261106` (`127146b21d`, read via the
   local mirror) adds `reverie/src/intercept.rs` with
   `InterceptStrategy {Trap, SudTrap, TrapAndPatch, PatchEager,
   PtraceExternal}` mapping to mechanisms — but **no strategy
   reaches `PageFaultGate`**, and `PatchEager` maps to
   `OfflineRewrite` (B5/V7), not to B4. §3 proposes the minimal
   join.
7. **Threads are the open lifecycle constraint.** In-guest
   thread creation refuses by name today; process-wide
   `tool`/`states` locks are "valid only while thread creation
   stays fail-closed" (`reverie-inguest/src/guest/host.rs:920-927`).
   W7's decided model (2026-11-06, museemu design via coord7):
   per-thread RPC connections opened lazily, held through the
   existing `native_state.rs` per-thread lease pattern (registry
   held only while moving a record into/out of a lease; no guard
   reaches Tool code, socket I/O, or a continuation —
   `reverie-inguest/src/guest/native_state.rs:6-11`);
   `CLONE_THREAD` admission stays behind a default-off capability
   bit. L6's registry must obey the same law (§2.5) or the two
   designs fork.
8. **Exec has a decided direction L6 must not contradict.**
   Ordinary in-guest exec refuses by name today; the decided
   direction is fork in-guest + ptrace re-bootstrap at exec
   (fork/exec plan `fd5a7815`, slices S-A…S-E, direction set
   2026-11-06). The Detcore native-exec fork-ban (convergence
   audit §7.2, F1–F3) survives only inside that domain. L6
   generations therefore live *within one image epoch* and are
   re-minted by re-bootstrap on exec (§2.4); L6 does not design
   a second exec engine. Convergence audit §5 target shape also
   binds: one in-guest core, interception adapters underneath;
   the gate is an adapter, not a new backend with private
   lifecycle plumbing.

## 2. (a) L6 protocol: mapping generations and revocation

### 2.1 Ownership: one authority, two consumers

There is exactly one mapping-generation authority per guest
process image: the **L6 registry** in the shared in-guest runtime
(proposed home: `reverie-inguest`, consumed by the liteinst
adapter — never duplicated inside the patcher library, per the
convergence target shape). It owns, for every executable extent
the runtime has an opinion about:

- the extent `[lo, hi)` at host page granularity
  (`sysconf(_SC_PAGESIZE)`, never a literal 4096);
- its **group** identity (the certified immutable extent — one
  object mapping generation; §4 defines the certificate);
- its **mapping generation** (`u64`, minted monotonically,
  never reused within an image epoch);
- its **gate state** (`Ungated | Sealed | Admitting | Admitted |
  Revoked`), generalising the spike's `SEALED/ADMITTING/ADMITTED`
  to many groups;
- the set of patched-site records and census inputs that depend
  on it (links into the existing `SITES`/`ARENAS` tables, not
  copies of them).

Both consumers read the same generation: the site patcher (today:
`SITE_STALE` flags) and the page gate (new). The registry is the
*only* writer of generations. Patch publication, gate admission,
and census (re)use each take a generation as a capability: work
performed under generation *g* is valid only while the registry
still reports *g* for that extent. This is the "real
proof/ownership, never a cached boolean" rule (L0 §7.1) made
operational: `adapter_mapping_replaced: bool` becomes
"certificate generation != current generation" (the spike's
`cert_gen` vs `mapping_gen` check), and the bool is deleted, not
kept in parallel.

**Invariant R1 (revocation before coverage — Astra P2).** No
thread may execute, patch, or census-prove bytes under a
superseded generation after the revoking mapping operation has
returned to the guest. Revocation is therefore *synchronous in
the mapping-syscall path*, before the guest resumes — not a lazy
flag the next fault happens to notice. The fault handler
re-checks the generation anyway (defence in depth, as the spike
does: stale check first), but the syscall-path revocation is
what makes the guarantee true between syscalls.

**Invariant R2 (one logical event, one Tool event).** Revocation
never synthesises, drops, or duplicates a Tool-visible syscall
event. The mapping syscall is observed exactly as today
(forward, observe result); L6 adds state transitions after the
same result, on the same dispatch paths. A revocation that
cannot be completed safely (e.g. re-seal `mprotect` fails) is a
fail-closed `R-CODE` termination with the guest stopped in the
handler/dispatch path, never a quiet continue.

### 2.2 What mints, what revokes (the total table)

A generation is **minted** when an extent first becomes known
executable (initial image census, successful `mmap`/`mprotect`
granting exec, `dlopen` load completing, exec re-bootstrap).
It is **revoked** (bumped, gate → `Revoked` → re-sealed or
dropped, dependent sites → `SITE_STALE`, dependent census →
unusable) by *any successful* operation that can change what
bytes an address fetches, what permissions they fetch under, or
which object they belong to. Failed syscalls (`result < 0`)
revoke nothing, matching today's early return.

| Guest operation (successful) | L6 action | Why |
|---|---|---|
| `munmap` overlapping a known extent | Revoke overlapped groups; drop fully-unmapped groups; split survivors (see partial-overlap rule) | Addresses may be reused by different bytes; stale jumps must not survive by assumption |
| `mmap` (incl. `MAP_FIXED`/`MAP_FIXED_NOREPLACE`) | Revoke any overlapped prior group; mint a fresh `Ungated` group for the new extent (admission decided by §3/§4, never by the `mmap` itself) | New bytes at old addresses are a new census problem (today: `mmap` already stale-marks, `runtime.rs:1607-1610`) |
| `mremap` | Revoke old range; revoke+remint at the new range (even a no-op in-place `mremap`) | Proven necessary by the `late_code` fixtures: a no-op `mremap` invalidates every site in the page |
| `mprotect` / `pkey_mprotect`: **any** protection or key change on a known/gated extent | Revoke. This deliberately widens today's rule (read-removal + pkey only): adding `PROT_WRITE` threatens immutability; adding/removing `PROT_EXEC` changes fetchability; removing `PROT_READ` breaks census custody — all three void a certificate | A certificate binds bytes *and* fetch permissions *and* unreadability assumptions; any change breaks at least one |
| `mprotect`/`mmap` creating a **writable alias** of a certified extent (same file offset / shared anon reachable writable) | Revoke the certified group, or refuse at seal time if the alias already exists (§2.3) | Bytes can change without any syscall on the RX mapping; generation alone cannot see it |
| `brk` growth into/over a known extent; `shmat` exec attach; `memfd`+`mmap` exec | Same as `mmap`: revoke overlap, mint `Ungated` | Same address-reuse hazard; enumerated so the table stays total, not syscall-number-lucky |
| `execve`/`execveat` (when admitted at all) | End the image epoch: every group, site, and certificate is discarded; successors start unadmitted (§2.4) | A successor image is a new first-instruction problem (I8) |
| `fork`/`clone` (process) | Child inherits a *copy* of the registry with generations preserved but gate state forced to `Sealed` until the child re-arms (§2.4) | COW preserves bytes, not handler liveness proofs |
| `clone` (thread, if/when admitted by W7) | No new generations; new threads fetch only through admitted groups or fault into the same gate | Gate is per-mapping, never per-thread (L0 §2.2, V6 Thr) |
| Direct guest writes to gated bytes (`PROT_WRITE` alias, `/proc/self/mem`, `process_vm_writev` self, `ptrace POKE` from a guest tracer) | Out of syscall mediation ⇒ prevented by construction or refused (§2.3); any such syscall targeting a gated extent that *is* mediated revokes first | Otherwise "immutable for a generation" is a hope, not a protocol |

**Partial-overlap rule.** Groups never straddle a revocation
boundary. If an operation overlaps part of a group, the *whole
group* revokes; surviving sub-extents re-mint as new groups
requiring fresh certificates. Splitting a certificate to keep
the untouched half "still valid" is forbidden: the certificate's
entry-closure proof was computed over the whole extent
(interior entries, dependency closure), and a half-extent has
no proof. This costs re-admission of survivors under churn —
accepted deliberately; churn-heavy workloads are exactly where
stale admission bites (Astra P2), and §5 budgets re-admission
at group, not page, granularity.

### 2.3 Immutability is enforced, not assumed

A group may be sealed (NX, awaiting first fetch) only if the
runtime can state *why* its bytes cannot change during the
generation:

1. The extent itself is not writable (`PROT_WRITE` clear) for
   the whole generation; any syscall that would make it
   writable revokes first (§2.2).
2. No other writable mapping of the same underlying pages is
   known in the guest. The registry tracks file identity
   (device, inode, offset) and shared-anon identity for known
   mappings; discovering a writable alias at seal time refuses
   (`R-CODE writable-alias`); creating one later revokes.
3. Self-inspection writes that bypass mapping syscalls
   (`/proc/self/mem` writes, `process_vm_writev`, guest-issued
   `ptrace` pokes) targeting a gated extent revoke before the
   write is forwarded when mediated; where the runtime cannot
   mediate them in the selected cell, the cell must not claim
   V6 (selection refuses, §3) — this is an admission-predicate
   question for the reviewer, flagged in §7, not waved through.

Where immutability cannot be established, the honest outcomes
are trap coverage (V2/B1 inside the page, slow and safe) or
`R-CODE` — never gated execution on mutable bytes. This is the
enforceable form of the L0/Astra rule that P9 shape indicators
cannot "prove" coverage: coverage comes from the gate +
certificate + immutability, or it does not exist.

### 2.4 Image epochs, fork, exec (no second exec engine)

Two counters, different lifetimes, never conflated:

- **Image epoch** — one per successfully armed process image.
  The existing `NativeThreadIdentity.image: u64`
  (`native_state.rs:39`, minted only via the admitted-facts
  constructor `from_admitted`) is its record. L6 does not mint
  image epochs; the launcher/re-bootstrap does.
- **Mapping generation** — many per epoch, minted by L6 as in
  §2.2. Every certificate, site record, and gate descriptor is
  tagged `(image_epoch, mapping_generation)`. A record from a
  prior epoch is dead on sight, even if addresses coincide.

**Fork.** The seccomp filter is inherited and patch state is
preserved across plain fork today (fork/exec plan §1); the L6
registry is COW-inherited the same way, with one change: every
`Admitted` group downgrades to `Sealed` in the child until the
child's runtime proves its gate handler is live (the liteinst
first-entry raw-fork profile, L0 §1.2, is the seed shape).
Generations are *not* bumped by fork itself — bytes did not
change — but the child's first fetch per group pays one fault to
re-prove liveness. vfork children (exit-or-exec only) never
admit new groups; they inherit the parent's admitted set
read-only, matching the existing translation semantics.

**Exec.** Until the re-bootstrap path (fork/exec plan S-A…S-E)
admits a successor, `execve` keeps refusing `R-EXEC` exactly as
today — L6 smuggles in no exec support. When re-bootstrap runs,
it ends the epoch: the registry is discarded wholesale (no
generation survives exec), the sealed runtime image is
remapped, and §3 sealing starts from an empty registry under
the new epoch. The retained execution plan is re-admitted, not
inherited (Astra B: retain the plan across fork/exec and
re-admit it; changing cells needs explicit transfer). The
Detcore native-exec domain (tickets, F1–F3) is untouched: its
childless-leader rules govern that domain, not ordinary V6
guests (fork/exec plan §4 supersession).

### 2.5 Concurrency and the W7 boundary (coordination, not a fork)

The registry obeys the same law as `native_state.rs`: **no
registry guard reaches Tool code, RPC/socket I/O, fault-handler
admission work, or a guest continuation.** Concretely:

- Mapping-syscall revocation (dispatch context, normal guest
  stack after sigreturn or in a hook trampoline) takes the
  registry lock only to bump generations, flip gate states, and
  stale-mark dependent sites — the same shape as today's
  `mark_site_range_stale` loop, no allocation through the guest
  allocator, no blocking RPC while held.
- The `SIGSEGV` gate handler (signal context) never takes that
  lock. It reads per-group atomics only (extent, generation,
  state, certified entries — the spike's `Gate` layout
  generalised to a fixed-capacity group table), in the spike's
  proven order: foreign-fault check → stale-generation check →
  already-admitted retry → certified-entry check →
  single-winner `Sealed→Admitting` CAS → one extent-wide
  `mprotect` → `Admitting→Admitted` release-store. Losers wait
  for the winner with a bounded, deadlock-argued wait (§3.3),
  never by running Tool/RPC code in the handler.
- Per-thread state stays W7's: each thread's RPC connection and
  callback lease (`native_state.rs`) are per-thread; the L6
  registry is per-process-image. The only shared edge is
  identity: a callback lease carries `(image_epoch)`; a gate
  fault carries faulting TID **for diagnostics only** — gate
  decisions never key on TID (per-mapping, §2.2). No new
  framing, no global gate RPC: admission is a local
  certificate check (§4), so threads never head-of-line-block
  each other on the coordinator to admit code.

**What needs museemu's answer (via museli-lt, §7 Q1–Q3):**
whether per-thread *fault accounting* (which thread admitted
which group, for teardown) belongs in the W7 lease record or
stays gate-private; who retires a thread's in-flight
`Admitting` CAS if that thread exits mid-admission; and whether
W7's lazy per-thread connection open needs a matching lazy
per-thread gate-handler arming check (alt-stack per thread) or
one process-wide handler suffices. This design proceeds on the
stated assumptions (gate-private accounting; winner-or-refuse,
never orphaned `Admitting`; one process handler + per-thread
alt-stack verification at thread admission) and will amend to
museemu's answers — the alternative, two registries with two
lease laws, is the fork to avoid.

Signal ownership is part of L6, not an afterthought. Three
runtime signals must co-exist under one ownership protocol:
`SIGSYS` (seccomp trap), `SIGTRAP` (liteinst concurrent-
publication router — refuses a pre-existing custom handler,
never replaces it), and `SIGSEGV` (new: the gate router). Rules:
the runtime owns all three routers or selection refuses
`R-SIGNAL` (probe P8); the gate router claims only faults whose
`si_addr` falls in a known gated extent *and* whose fault is an
instruction fetch against a sealed group — every other fault is
foreign and must chain to the guest's disposition / Tool signal
protocol exactly as without the gate (never swallowed, never
converted to a gate refusal — the spike's `EXIT_FOREIGN` model
is a spike simplification, explicitly not the product rule);
guest attempts to replace the three dispositions at runtime go
through the existing signal-admission guards and refuse by name
where they would steal a router.

### 2.6 Refusals added by L6 (all `R-CODE` family, named)

Reuse the landed taxonomy (`RefusalCode`, `selection.rs`);
add detail strings, not new top-level codes, unless review
says otherwise: `R-CODE stale-generation` (certificate behind
current generation), `R-CODE interior-entry` (first fetch at an
uncertified offset in a sealed group), `R-CODE writable-alias`,
`R-CODE mprotect-failed` / `reseal-failed`, `R-CODE
admission-stuck` (bounded wait exhausted), `R-CODE
uncertified-group` (no B5 certificate and no trap fallback in
the selected cell). Selection-time siblings stay as they are:
`R-SIGNAL` (router unavailable), `R-WX` (mapping policy forbids
seal/grant transitions), `R-WINDOW`, `R-EXEC`, `R-UNBUILT`.
Mid-run gate refusals happen with the guest stopped in the
fault/dispatch path and carry `(extent, generation,
cert_generation, fault_addr)` in the log so "why won't it run"
is answered without reading source (L0 §3.3 explainability).

## 3. (b) V6 as a selectable Reverie interception strategy

### 3.1 The minimal W5 join: one variant, no fork

musearch's W5 slice 1 (`reverie/src/intercept.rs` on branch
`musearch/w5-intercept-20261106`) is the operator surface; L3
extends it by exactly one variant, stacked *after* W5 lands
(the enum is not in the slot `main` tree yet — sequencing in
§6):

```rust
// Proposed delta to musearch W5 (design sketch, not code):
pub enum InterceptStrategy {
    // … Trap, SudTrap, TrapAndPatch, PatchEager, PtraceExternal (unchanged) …
    /// B4: seal certified groups NX; first fetch admits the group,
    /// then patch (B3) / trap (B1) inside admitted pages (V6).
    PageGate,  // env value: "page-gate"
}
// mechanism(): PageGate => Mechanism::PageFaultGate (already exists)
// resolve(): PageGate refuses Unavailable off Linux x86-64 / kernel < 6.x;
//            no SUD-probe downgrade (gate correctness is SUD-independent, I6).
```

Everything else about W5 is inherited unchanged: config-selected
once per run (builder wins over env), unknown/non-UTF-8 values
refuse by name before the guest starts, the chosen value is
logged in the run banner, strategy is interception-only
(activation, transport, and exec handling stay orthogonal
selections). `PatchEager` keeps meaning B5/V7
(`OfflineRewrite`); `PageGate` must not be spelled as a
`PatchEager` flag — B4 and B5 are different mechanisms with
different refusal surfaces, and overloading one value would
re-create the silent-divergence bug W5 exists to kill.

Reconciliation with today's surfaces (no new env soup):
`REVERIE_LITEINST_SITE_PATCHING` remains the low-level preload
toggle (`0`=trap-only, `1`=patching) for the existing V4/V5
cells; `BackendKind::{Liteinst,InGuestTrap}` in the examples
keeps mapping to it. `REVERIE_INTERCEPT=page-gate` is honoured
only by a launcher path that can seal before first fetch
(§3.2); requesting it on the preload path refuses by name
before guest start (`strategy-unavailable-on-this-backend`,
same discipline as W5 rule 2) — never silently degrades to
`TrapAndPatch`.

### 3.2 Activation constraint: no seal, no V6

Sealing must happen before the guest's first fetch from each
group, which only A-stop-class activation can promise for the
initial image (A1 `PtraceExecStop` with the in-guest runtime
armed at the exec stop — the stage-1a machinery in
`reverie-inguest/src/launcher/exec_stop/`, whose production
release transition is still the known "Commit 3" gap) or,
later, A3 custom loader. Therefore:

- The V6 `ImplementedCell` (`DesignCell::V6`,
  `Mechanism::PageFaultGate`, `Placement::InGuest`,
  `WindowGuarantee::Zero`, new `CellConfig` variant for the
  gated in-guest configuration) is registered **only in the
  slice that also wires the A-stop arming path** (§6 S4).
  Until then `page-gate` parses, logs, and refuses
  `R-UNBUILT` — the enum may name it, selection must refuse it
  (W5's own rule for not-yet-landed strategies).
- A preload ("late seal") V6 variant is **rejected** in this
  design: by constructor time, `ld.so`/IFUNC/early-constructor
  fetches already executed unsealed (I1). Tools that declare a
  window already have V4/V5; a windowed gate would claim V6's
  Dyn guarantee without its premise.
- Late code (`dlopen`, fresh exec mappings after start) is
  sealed at the mapping-syscall boundary instead: a successful
  exec-granting `mmap`/`mprotect` mints `Ungated`, and before
  the guest can fetch from it the runtime either seals it
  (certificate available, §4) or leaves it to the completeness
  trap / refuses per the cell contract. The window between
  mapping return and seal is closed because revocation/sealing
  runs synchronously in that syscall's dispatch (Invariant
  R1), on the calling thread, before any guest fetch resumes.

### 3.3 Gate runtime shape (steady state has no fault-path work)

Per group, the lifecycle is: **census+certify** (§4, off the
fault path) → **seal** (`PROT_READ`, NX; one `mprotect` per
group at arm time) → **first fetch faults** → handler verifies
(generation current, fault address is a certified entry,
certificate custody per §4) → **publish patches** for the
group's certified sites (quiescent publication while the group
is still NX and the faulting thread is stopped in the handler
— no concurrent fetcher exists for this extent, which is
exactly liteinst's quiescent precondition) or arms trap
fallback per site → **grant** (one extent-wide
`mprotect RX`) → resume with the kernel `ucontext` untouched.
Second and later fetches in the group run at V3/V2 cost with
no fault. Sites the certificate cannot patch keep the
completeness trap (seccomp `SIGSYS` → lazy trap-and-patch as
today); a group with no certificate and no trap fallback in
the selected cell refuses `R-CODE uncertified-group` at seal
time, not at fetch time.

Handler law (from the liteinst skill invariants, restated as
gate requirements): async-signal-safe operations only; no guest
allocator, no Tool code, no coordinator RPC from signal
context; fixed-capacity group table (exhaustion refuses by
name, predictably); text permissions restored on every path;
modified instructions made visible to executing cores
(sync-core serialisation counted in §5, not hidden). The
loser-wait for racing first faults is bounded spin-then-refuse
in v1 (spike shape) with the bound justified in §5; a
futex-based wait is a later optimisation only if measurement
shows spin burn under thread storms — and threads are refused
until W7 admits them anyway (§2.5), so v1's racing case is
fault redelivery, not an 8-thread storm.

### 3.4 What V6 guarantees (cell contract to register)

C: best in-place story — CPUID/RDTSC (and wider classes the
certificate enumerates) patched/trapped before first execution
in admitted groups; elsewhere as V2. Thr: per-mapping gate ⇒
new threads observed from first fetch *once W7 admits threads*
(until then `ThreadsBound::SingleThread`, thread creation
refuses as in V4/V5 — registering more would be false). Sig:
§2.5 router protocol. Fork: §2.4 (followed, child re-seals).
Exec: `Refused` until re-bootstrap S-B/S-C admit successors,
then `RearmedSameCell` — the envelope is widened by a later
slice with its own witnesses, never by editing the note field
alone. Dyn: certified dynamic code admitted; arbitrary JIT
`R-CODE` (separate corpus cases, L0 §5.3). Debugger:
`ExclusiveOnly` (A-stop needs ptrace seize; V9 is L5's cell).

## 4. (c) Ahead-of-time scan (B5): group certificates

B5 here means the **scan+certify front end** for V6, not the
V7 offline-rewritten image. The guest file is never rewritten
by L3; the scan produces a sidecar **group certificate** the
gate checks locally. V7 (emission/elf-rewrite candidates,
Partial today) remains a separate cell that may later consume
the same census engine; nothing in L3 depends on turnkey V7.

### 4.1 Certificate contents (bound to bytes, entries, generation)

One certificate per group (one immutable executable extent of
one object), carrying everything admission needs without
re-reading maps or re-decoding on the fault path:

- **Extent identity:** file device + inode + file offset +
  extent length (or anon identity for sealed
  non-file extents), plus the mapping generation it was issued
  for (re-bound at seal time to the live generation; a
  certificate never carries permission to execute — it carries
  proof *about* a generation).
- **Content binding:** cryptographic hash over the exact RX
  extent bytes (spike: sha256 over exact extents), plus the
  stat custody tuple (device, inode, size, mtime/ctime) for
  the fast-path check in §4.3.
- **Entry closure:** the certified entry set — function starts,
  direct branch/call targets, return addresses, landing pads,
  RIP-relative `lea` results, relocated code pointers — i.e.
  exactly what today's census already enumerates
  (`reverie-liteinst/src/liteinst_census.rs`, `.eh_frame_hdr`
  walk). First fetch at any other offset is
  `R-CODE interior-entry`, full stop.
- **Site plan:** per syscall/CPUID/RDTSC site: patchable
  (with the tactic outcome: pun / proved relocation /
  straddler bailout) or trap-fallback — computed by the same
  decision tree the runtime patcher uses, so offline and
  online verdicts cannot diverge silently; any site the plan
  cannot decide is trap-fallback or the group is uncertified.
- **Dependency record:** which other mappings the entry proof
  read (relocation sources, unwind metadata extents) so L6
  can revoke the certificate if a *dependency* mapping is
  replaced or made unreadable — generalising today's
  `adapter_mapping_replaced` / census-input invalidation.

Reuse, do not reinvent: the census engine is
`liteinst_census.rs` (already the runtime's proof that no
control transfer lands inside a patch window; residuals are
documented there and inherited here verbatim: no
`PT_GNU_EH_FRAME` ⇒ no census ⇒ no patching; anonymous/JIT
mappings get no census). The liteinst-side prior art for
versioned entry facts is `liteinst2/crates/liteinst-admission`
(all-start scanning, versioned facts — a *model* that "never
changes executable memory and is not a live admission gate",
L0 §1.1); L3's certificate is the live-gate form of those
facts, built by Reverie's census, not by importing the model
crate as an oracle.

### 4.2 When scanning runs (never on the fault path)

1. **Initial image:** at arm time, while the guest is stopped
   at the exec stop (A-stop) — scan+certify+seal all initial
   RX groups before handing off to `ld.so`. Cost is startup
   cost, reported separately (§5), never hidden.
2. **Late objects (`dlopen`):** scan+certify+seal synchronously
   in the mapping-syscall dispatch that makes the object
   fetchable (§3.2), before resume. This is dispatch-context
   work (allocation allowed through runtime arenas, never the
   guest allocator in signal context), budgeted in §5.
3. **Across runs (cache):** certificates may be cached keyed
   by content binding (device/inode/hash) so repeat runs skip
   the cold scan. A cache hit still re-binds to the *live*
   mapping generation and re-checks custody (§4.3); a cache is
   a memo of proofs, never an admission by itself. Cache
   poisoning (same inode, changed bytes) is stopped by the
   content binding, not by trust in the cache.

Anonymous/JIT extents have no file identity and no unwind
table: they are **uncertified by default** in V6 v1. Certified
dynamic code (a JIT that cooperates: emits into a sealed
extent, declares its entries, and requests certification
before first fetch) is the L0 §5.3 "certified" corpus case and
needs its own admission API — scoped as a follow-on slice
(§6 S5), not smuggled into v1. Everything else refuses
`R-CODE` or runs under the trap, per the selected cell.

### 4.3 Custody check: stat fast path, hash as the judge

Spike numbers frame the policy: stat-identity custody ≈7 µs
vs cold sha256 ≈1.63 ms median (n=10) for the `ls` closure —
a ~200x gap that decides whether sealing is cheap or dominant.
Policy (for reviewer decision, §0 item 3):

- **Seal time (group first seen):** full content hash +
  census. No shortcut; this is where trust is created.
- **Re-admission of the same generation** (fork child,
  re-seal after revoke+fresh-cert): stat custody (device,
  inode, size, timestamps) may short-circuit re-hashing
  *only* while immutability (§2.3) held continuously for that
  generation. Any revocation, alias discovery, or custody
  mismatch forces a full re-hash + re-census.
- **Cache hits across runs:** stat custody selects the cached
  certificate; the live extent's hash is still verified at
  seal time before the first grant. (Cheaper alternative —
  trust stat alone across runs — is rejected in §7: inode
  reuse and timestamp granularity make it a cached boolean in
  disguise.)

The reviewer trade-off is explicit: if review judges even
seal-time hashing too costly for large closures, the lever is
fewer/larger groups and better caching — not weaker binding.
Weakening the binding to meet a speed target is goalpost
movement and out of bounds for this design.

## 5. (d) Performance plan against the ≤1.5x DynamoRIO-null gate

**The gate (owner ruling 2026-11-04 10:42 PM ET, L0 §7.1):**
≤ 1.5x DynamoRIO-null wall time, same machine and workload,
median of 10 runs. It is the M3 exit gate and L3's target;
correctness landings are **not** gated on it. Every performance
claim reports DR-null ratio + run count + machine, or it is an
anecdote. L9/musebench owns the verdict; this lane owns
producing numbers in that form from the first runnable week.

### 5.1 The arithmetic (measured inputs, derived budgets)

Measured anchors (EPYC 9D85 unless noted): `ls` native
4.17 ms, DR-null 26.3 ms ⇒ 1.5x budget = 39.45 ms, headroom
over native = 35.28 ms *(derived)*. Per-page admission at
5,393 pages cannot fit (6.54 µs budget < 8.33 µs NX cycle,
L0 §6.3). Grouped admission changes the denominator: `ls` =
6 groups; at the spike's 1.57 µs fault+handler+grant, total
fault-path cost ≈ 9.4 µs *(derived: 6 × 1.57 µs)* — three
orders of magnitude inside headroom. **That arithmetic is
necessary, not sufficient.** The 1.57 µs excludes sealing,
sync-core, certificate creation/validation, and patch
publication; the budget table below accounts for every term
and marks what is still UNMEASURED. No slice may quote the
9.4 µs figure without the exclusions attached.

| Cost term (per run, `ls`-class) | When paid | Anchor today | Budget discipline |
|---|---|---|---|
| Cold census+hash (initial image) | Arm time (guest stopped) | sha256 1.63 ms median (n=10); census time UNMEASURED | Report as startup cost; cache across runs (§4.2); must not move to fault path |
| Stat custody per group | Seal / re-admit | 7 µs (spike) | Negligible; still counted |
| Seal (`mprotect` NX per group) | Arm time / late-map dispatch | UNMEASURED as a group primitive | One syscall per group; measure in S2 spike before integration |
| First-fetch fault+handler+grant | Once per group | 1.57 µs median (2-page model, excl. sealing/sync-core) | Re-measure at real group sizes + 4K/64K pages in S2 |
| Patch publication per site | Inside admission (quiescent) | 0.49 µs aligned / 4.15 µs crossing (N=20,000, liteinst micro-costs) | Sum over certified sites; trap-fallback sites pay trap cost instead, counted separately |
| Sync-core / visibility | Publication + grant | Included in the refuted 8.33 µs NX cycle; isolated cost UNMEASURED | Isolate in S2; never hide inside "handler" time |
| Steady-state syscalls | After admission | As V3/V2 (detour or trap); whole-program UNMEASURED | The M3 bake-off number; per-trap anchors: seccomp ≈880 ns, SUD ≈879 ns, raw ≈69 ns (musearm1, loaded host, indicative) |
| Revocation+reseal under churn | Mapping-syscall dispatch | UNMEASURED | Budgeted per group re-admission; churn corpus (§6) must report it, not average it away |

### 5.2 Invariants that keep the design inside the gate

1. **Zero maps reads, zero decoding, zero hashing on the fault
   path.** The handler does atomic loads, integer compares,
   one CAS, patch publishes from the precomputed site plan,
   and one `mprotect`. Anything else is a design violation,
   found by review or by the S2 primitive breakdown.
2. **Admission count = group count, not page count.** Grouping
   follows object mapping generations (spike census: 6/575,
   3/418, 4/649). Any change that splits groups toward pages
   (finer certificates, partial-overlap splitting beyond §2.2)
   must re-derive the budget in its PR description.
3. **64K pages are first-class.** All extents, seals, and
   grants use `sysconf` page size; S2 measures 4K and 64K
   (dual coverage is a decided hard requirement). The spike's
   literal 4096 does not survive into product code.
4. **Refusals are never speed successes.** A run that refuses
   (`R-CODE`) is reported as refused, with wall time, in every
   table — never dropped, never reclassified as "unbuilt"
   (Astra goalpost discipline). Offline preparation and
   cold/warm runs are reported separately.
5. **Floor before bar.** M2 floor: `ls`/`true`/shell *complete*
   under V6 (today's page-gated shape dies at timeouts). M3
   bar: ≤1.5x DR-null median-of-10. A design that cannot state
   how it removes per-page work does not proceed (L0 §7.1) —
   this note's answer is §4 certificates + §3.3 fault path;
   review should attack exactly that claim.

### 5.3 Measurement protocol (per slice, from S2 on)

Each runnable slice reports, on one named machine: workload
(`true`, `ls`, shell chain, plus the churn/JIT corpus in §6),
median of exactly 10 timed runs with raw samples + IQR +
warmup policy, DR-null ratio on the same machine/workload,
per-admission primitive breakdown (seal, fault+handler,
publish sum, sync-core, grant), and refusal counts. Matched-
tool comparisons (same Tool on V2/V3/V6) accompany, never
replace, the DR-null ratio. Numbers from the two-page spike
model are labelled MODEL until S2 re-measures at real extents.

## 6. (e) Slice plan with tests (all slices post-approval)

**Stacking rule.** This design commit sits on
`musepage/pagegate-design-20261106` at `main` `778a446c`.
Every later *code* slice stacks after the C1+S1 follow-on
(branch `liteinst/c1s1-followon-20261106` in the slot: C1 PR1
mapped transport + S1 native-exec admission + S2a-1 witness),
per the clean-stack rule — code lands on one clean Reverie
stack, in this order, each slice independently reviewable and
revertible. W5 (`musearch/w5-intercept-20261106`) lands before
S3 by construction. No slice below starts until §0's decisions
are approved; sizes are estimates for sequencing, not evidence.

| Slice | Content (surfaces) | Tests / exit evidence |
|---|---|---|
| **S0** (this note) | Design only. No code. | Review routed by museli-lt; §7 questions answered or carried as named risks. Exit = approved design SHA. |
| **S1 L6 registry, no gate** | `reverie-inguest` L6 registry (groups, generations, gate states; `sysconf` pages); rewire `observe_mapping_generation` to mint/revoke per §2.2 (incl. the widened `mprotect` rule) and delete `adapter_mapping_replaced` in favour of generation compare; site/census consumers read `(epoch, generation)`. Behaviour for V4/V5 unchanged (no sealing yet). | Unit: mint/revoke/partial-overlap/failed-syscall table (§2.2 each row). Regression floor: `cargo test -p reverie-liteinst --test strace -- --test-threads=1`, `--test rpc_tool`, `--all-features`; signature `calls=32 traps=1 hooks=32` preserved; `late_code` churn fixtures (no-op `mremap`, dlopen CPUID/RDTSC) green. Adversarial: alias-write + `MAP_FIXED` overwrite must stale-mark (new tests, named). |
| **S2 Gate primitives spike→library** | Generalise the spike to N groups, `sysconf` pages (4K+64K), foreign-fault chaining (replacing spike `EXIT_FOREIGN`), bounded loser-wait; still a standalone library + witnesses, **no backend wiring**. Measure seal/sync-core/grant primitives at real extents. | Port the 5 witnesses (`tests/grouped_admission.rs` shapes: admit/interior/revoke/readmit/concurrent) to N-group + foreign-chaining + 64K variants; bench mode extended to print the §5.1 primitive breakdown. Exit = witnesses green + primitive table (machine+n+medians) reviewed before any integration. |
| **S3 B5 certificates + `PageGate` value (refusing)** | Certificate builder over `liteinst_census.rs` (hash, entry closure, site plan, dependency record); custody policy §4.3; `InterceptStrategy::PageGate` + `page-gate` env parse + `mechanism()`/`resolve()` stacked on landed W5. Selection still refuses `R-UNBUILT` (no cell registered). | Certificate determinism (same bytes ⇒ same cert), staleness (byte flip ⇒ hash/custody miss), residuals inherited (no-EH-frame ⇒ uncertified, anon ⇒ uncertified); W5 test quartet for the new value: parse table, mismatch refusal (preload/aarch64 refuse by name), banner logging, builder-wins. No guest runs gated yet. |
| **S4 V6 integration + cell registration** | A-stop arming: census+certify+seal initial groups at exec stop (closing the stage-1a release gap for this path); gate handler + quiescent publish + grant (§3.3); late-map sealing (§3.2); fork re-seal (§2.4); register the V6 `ImplementedCell` with the honest §3.4 envelope (SingleThread, Exec Refused). | Conformance (L0 §5): recorder/counter/mutator differential V6 vs V1/V2 on the frozen non-exec corpus (static/dynamic/PIE hello, `ls`, signal-dense, nondet loop, dlopen churn); refusal oracle (exact `R-*` names: interior, stale, alias, uncertified, window); firstprobe zero-window on `hello`+`ls`; foreign-`SIGSEGV` chaining witness (guest handler still receives its faults). Perf: §5.3 protocol, floor = completes; DR-null ratio reported, not yet gated. |
| **S5 Hardening + widenings (separate approvals)** | Exec successors via re-bootstrap S-B/S-C (envelope → `RearmedSameCell` only with sh-chain witness); W7-threaded admission when W7 lifts the capability bit; certified-dynamic-code API; certificate cache across runs; futex loser-wait if measured necessary. Each is its own slice + review; none is implied by S4. | Per widening: its own witnesses (exec chains incl. static + fork-child; thread storm first-fault racing; certified-JIT admit vs arbitrary-JIT `R-CODE` as separate cases) + §5.3 re-measurement toward the M3 ≤1.5x bar (musebench verdict). |

**Rollback.** S1 is behaviour-preserving for V4/V5 (revert =
prior flags; generations are additive until the bool deletion,
which is the last commit of S1). S2/S3 add no selectable
behaviour. S4's cell registration is a single commit: reverting
it returns `page-gate` to `R-UNBUILT` without touching the
registry or certificates. No slice changes Hermit; Hermit
consumes V6 later via pin bump, sequenced after S4 admission
exists (convergence audit §7.4 sequencing warning).

## 7. Alternatives rejected, risks, open questions

**Alternatives rejected (with reasons, for the record).**
Per-page fault+scan (refuted by §1 fact 4 arithmetic).
Cached-boolean / stat-only admission across runs (inode reuse
+ timestamp granularity; violates "never a cached boolean").
Preload late-seal V6 (structural window, I1; §3.2).
`PageGate` as a `PatchEager` flag (conflates B4/B5; §3.1).
Trusting the spike's foreign-fault exit as product behaviour
(guest `SIGSEGV` fidelity is contractual; §2.5). A second
gate-private generation counter beside site staleness (two
authorities fork under churn; §2.1). Requiring SUD for gate
discovery (I6; seccomp is the decided default on both arches,
SUD x86-only optional).

**Top risks.** (1) *Stale admission via an unmediated write
path* (§2.3 item 3) — if any guest-reachable write to gated
bytes escapes mediation in the A-stop cell, V6's premise
fails; S4 must enumerate the write paths, not sample them.
Highest-risk validation step: the S1 alias/`MAP_FIXED`
adversarial tests plus S4's write-path enumeration review.
(2) *Seal-time cost on large closures* (census UNMEASURED) —
could dominate startup even with cheap faults; S2/S3 measure
before S4 commits to the envelope. (3) *A-stop release gap* —
S4 depends on production exec-stop arming that is still
Partial; if it slips, S1–S3 still land value (registry +
certificates serve V3/V5 soundness) and V6 stays `R-UNBUILT` —
honest, not blocked-in-secret. (4) *Spin-wait burn* is
deliberately deferred behind thread refusal; lifting threads
without re-measuring the loser-wait would be a silent perf
regression.

**Open questions (for museli-lt / reviewers; none blocks S0).**
Q1–Q3 for museemu via museli-lt: fault-accounting home
(lease vs gate-private), mid-admission thread-exit retirement,
per-thread alt-stack/handler arming (§2.5). Q4: does the
reviewer accept stat-custody re-admission within a generation
(§4.3), or require re-hash on every re-seal? Q5: fixed group-
table capacity + overflow policy (proposed: refuse
`R-CODE` by name; capacity sized from S3 census data, not
guessed here). Q6: should `R-CODE` detail strings become typed
variants in `RefusalCode` now, or stay detail strings until a
second consumer needs them? Each question names its default
(this note's text) so review can amend by exception.

## Sources (inspected this session)

- `ai_docs/plans/2026-11-04-liteinst-reverie-architecture.md`
  (§§1.1–1.2, 2.1–2.3, 3, 4.1, 6.1–6.3, 7.1) and
  `ai_docs/plans/2026-11-04-liteinst-reverie-full-court-press-plan.md`
  (L3/L6 lanes, M2/M3 gates).
- `ai_docs/plans/2026-11-05-inguest-backend-convergence-audit.md`
  (§§1–5 target shape; §7 native-exec domain boundary).
- `ai_docs/plans/2026-11-06-inguest-fork-exec-state-and-plan.md`
  (fork/thread/exec state; re-bootstrap S-A…S-E) and
  `ai_docs/plans/2026-11-06-arm-literoot-full-court-press.md`
  (W5 enum §4; W7 per-thread decision; seccomp-default,
  64K-page, kernel-6.x decisions).
- Reverie PR https://github.com/rrnewton/reverie/pull/1290
  (merged; head `cad4ebfc`; spike `acb46c8c` measurements:
  6/575, 1.63 ms n=10, 7 µs, 1.57 µs excl. sealing/sync-core;
  5/5 witnesses). PR bodies for components:
  https://github.com/rrnewton/reverie/pull/1283 (selection),
  https://github.com/rrnewton/reverie/pull/1285 (probes).
- Slot code at `778a446c`: `reverie/src/selection.rs`,
  `reverie/src/probes.rs`,
  `reverie-liteinst/src/runtime.rs` (`:1601` mapping
  generations, `:3013,3264,3470` observe paths),
  `reverie-liteinst/src/liteinst_census.rs`,
  `reverie-liteinst/src/bin/grouped_admission_spike.rs`,
  `reverie-liteinst/tests/grouped_admission.rs`,
  `reverie-inguest/src/guest/native_state.rs`,
  `reverie-inguest/src/guest/host.rs:920`,
  `reverie-examples/src/shared_host.rs:31`; W5 branch
  `musearch/w5-intercept-20261106` `127146b21d`
  (`reverie/src/intercept.rs`); C1+S1 follow-on branch
  `liteinst/c1s1-followon-20261106` (stacking base for code).
- Slot skills applied: `liteinst-binary-instrumentation`
  (handler/patch invariants), `liteinst-tool-lifecycle`
  (ownership boundaries), `liteinst-testing` (regression
  shapes and evidence discipline).
