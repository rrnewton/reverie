# The Reverie `Backend` trait and the portable test corpus

**Status:** decision document. Written for the owner to review and decide.
**Author:** `[impl agent, claude-opus-5] [dev-hermit fleet, devbig]` (agent `green-baseline`)
**Date:** 2026-08-08
**Reverie read SHA:** `f7e02e918570392426eb1a3e6a7978571db6c217` (pinned once, reused for every
claim below; the earlier recon this builds on read `0384d673319bf139cf7d71fda820245a9266364d`,
and every HIGH-confidence claim here was re-verified at the newer SHA).

---

## 0. How to read the confidence grading

This document mixes claims read directly out of source with claims produced by a mechanical
pass that is **known to misclassify in both directions**. Those are not the same kind of fact
and the difference is load-bearing for what you can decide today. Every claim carries a grade:

- **HIGH** — read directly from the named file and line at the pinned SHA. Re-derivable by one
  command. If one of these is wrong, I made a reading error, not an estimation error.
- **MEDIUM** — produced by pattern-matching over the tree. The classifier behind these numbers
  produced four different answers before it was abandoned (§6). Directionally useful, not
  citable as a count.

**Nothing in this document is graded HIGH unless I opened the file.** Where a number is MEDIUM
and the decision would turn on its exact value, that is called out in place.

---

## 1. The finding in one paragraph

Reverie's `Backend` trait — the abstraction a tool author writes against without naming a
backend — declares **exactly one method**, and that method returns only `(ExitStatus,
GlobalState)`. A test that needs the guest's **stdout**, a **preload DSO**, or **instrumentation
stats** cannot express that need through the trait, so it reaches for a backend's *inherent*
convenience wrapper instead. Those wrappers are therefore **not laziness — they are filling real
gaps in the front door.** The consequence is that the standing instruction *"port these tests to
the front door"* is, for that population, **currently unsatisfiable**: carrying it out against
today's trait would produce tests that compile, pass, and no longer check the thing they were
written to check. That is strictly worse than leaving them alone, because a test that has
stopped checking still reports green.

---

## 2. The front door, read from the trait body

**Confidence: HIGH.** Read from `reverie/src/backend.rs` at the pinned SHA.

The consumer-facing contract is four traits plus supporting re-exports:

| Surface | Location | Role |
|---|---|---|
| `Tool` | `reverie/src/tool.rs:197` | decides **what** to do on a trappable event — "what most users write" |
| `GlobalTool` | `reverie/src/tool.rs:118` | state shared across the guest tree; supplies `Config` |
| `Guest<T>` | `reverie/src/guest.rs:57` | the view handed to handlers: regs, memory, `inject`/`tail_inject` |
| `Backend` | `reverie/src/backend.rs:147` | decides **how** events are trapped and how the tool is run |

`pub trait Backend` (line 147) declares one function and nothing else — the whole execution
entry point of the abstraction:

```rust
#[async_trait(?Send)]
pub trait Backend {
    async fn run<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ExitStatus, T::GlobalState), Error>
    where
        T: Tool + 'static;
}
```

So the entire portable surface for *running* something is `SomeBackend::run::<MyTool>(cmd, cfg)`.
**The only backend-specific token in a genuine front-door test is the receiver type.** Swap
`E9patchBackend` for `PtraceBackend` and the test is ported. That is precisely the structural
criterion in the stated priority — *agnostic structure, not agnostic outcomes* — and it is why
per-backend differences in expected syscall counts are irrelevant to the classification.

### 2.1 The trait already documents the gap as a deliberate choice

This matters for how you weigh the two options, so it is quoted rather than paraphrased. The
trait's own doc comment, immediately above `pub trait Backend`:

> Real backends typically layer extra, backend-specific capabilities on top — for example
> `reverie-ptrace` additionally offers output capture, a GDB server, and spawning a *function*
> (rather than a `Command`) under instrumentation. Those live on the backend's own builder type;
> **this trait only fixes the common denominator every backend shares** so that tools — and
> readers — have one explicit definition of "what a Reverie backend is".

**Option (B) is therefore not a retreat; it is the documented design intent, already written
down.** Anyone arguing for (A) is arguing to change a stated decision, not to fill an oversight.
I think (A) is still right, for reasons in §7, but the argument has to clear that bar honestly.

### 2.2 Who actually implements it

**Confidence: HIGH** (`grep "impl Backend for"`, excluding `vendor/` and two doc-comment
examples in `reverie/src/`).

**Three** implementors, all real:

- `PtraceBackend` — `reverie-ptrace/src/backend.rs:47`
- `E9patchBackend` — `reverie-e9patch/src/backend.rs:1047`
- `LiteinstBackend` — `reverie-liteinst/src/backend.rs:587`

**KVM, the DBT runtime, and the SaBRe adapter do not implement the trait.** Per `BACKENDS.md`
this is an API-status distinction, explicitly *not* a claim that they are simulations — they run
real `Tool`/`Guest` code, just without a common launch contract. The practical consequence for
porting is blunt: **for a KVM test there is no receiver to swap *to*.**

---

## 3. What the wrappers are actually filling — and the split that decides this

**Confidence: HIGH.** Signatures read directly.

Every concrete backend ships inherent methods that are not on the trait. They look like the front
door and are not: `LiteinstBackend::run_host_with_output_and_preload::<T>` cannot be ported by
swapping the receiver, because no other backend has that method. **That dividing line is
invisible unless you read the trait body**, which is the single most useful fact in this document.

Grouping the wrappers by *what capability they add* — rather than by backend — separates the
question into two halves that have **different answers**:

### 3.1 Output capture — already converged, independently, on one signature

This is the crux, and it was the surprise of the investigation. The three trait implementors have
each grown an output-returning variant, and **all three arrived at the identical signature**:

| Backend | Method | Returns |
|---|---|---|
| e9patch | `run_with_output<T>` (`reverie-e9patch/src/backend.rs`) | `Result<(Output, T::GlobalState), Error>` |
| liteinst | `run_with_output_and_preload<T>` (`reverie-liteinst/src/backend.rs`) | `Result<(Output, T::GlobalState), Error>` |
| ptrace | `wait_with_output` (`reverie-ptrace/src/tracer.rs:1402`) | `Result<(Output, G), Error>` |

`Output` is not three types. It is one shared type — `reverie_process::child::Output`, re-exported
at `reverie-process/src/lib.rs:40` and reachable as `reverie::process::Output`, a crate every
backend already depends on.

And the reference backend is the clearest case of all. `reverie-ptrace` exposes the two operations
**side by side on the same type**, one line apart in the same file:

```rust
pub async fn wait_with_output(mut self)      -> Result<(Output, G), Error>      // tracer.rs:1402
pub async fn wait_discarding_output(mut self) -> Result<(ExitStatus, G), Error>  // tracer.rs:1446
```

**The trait's sole method is the `wait_discarding_output` half of a pair the reference backend
already implements in full.** So the gap does not exist because output capture resists
abstraction. It exists because the narrower of two already-uniform operations was the one lifted
into the trait. Note also that `Output` carries the exit status, so the richer method **subsumes**
the current one rather than sitting beside it — the two are not independent capabilities.

*(One nuance, stated because it is a real cost and not a footnote: ptrace's variant lives on
`Tracer`, reached via `TracerBuilder` (`reverie-ptrace/src/tracer.rs:1831`, `spawn` at :2223), not
as an inherent method on `PtraceBackend` — `reverie-ptrace/src/backend.rs` has no public inherent
methods at all. Implementing a trait-level `run_with_output` for ptrace means routing through
that builder inside the impl. That is real work. It is not a design question: the target
signature already exists and already matches.)*

Even the non-trait backends have the capability — `reverie-kvm/src/vm.rs` has
`run_static_elf_captured` returning `(i32, Vec<u8>, Vec<u8>)`. Different shape, same capability,
which is evidence that output capture is universal to what these backends do rather than specific
to how any one of them works.

### 3.2 Preload and stats — genuinely mechanism-specific, and not converged

The picture is the opposite here, which is why this document does not recommend one answer for
everything.

- **Preload DSO** — pervasive in liteinst (`run_with_preload`, `run_host_with_preload`, and every
  `*_and_preload*` variant) and present in e9patch's direct-mode variants. **`reverie-ptrace`'s
  backend has no preload concept**; it intercepts via seccomp and does not need a preload DSO to
  do its job. A trait-level preload parameter would be meaningless for the reference backend.
- **Stats** — `*_and_stats` variants exist in **liteinst only** (`run_with_preload_and_stats`,
  `run_host_with_output_and_preload_and_stats`, and siblings). One backend is not an abstraction.

Forcing either of these onto the trait means every other implementor supplies a stub. A stub that
returns `Ok` with nothing is the **absence-reads-as-success** failure — the test passes because
the capability was silently absent. A stub that returns `Err` is more honest but means the trait
now advertises a method most implementors refuse, which is a worse contract than not having it.

**This is the load-bearing distinction in the whole document:** the gap is not one gap. It is one
capability that is already uniform across every implementor, plus two that are genuinely
mechanism-specific.

---

## 4. How large is the affected population

### 4.1 What goes through the front door today

**Confidence: HIGH.** Counted by direct inspection of the only four files containing any
`Backend::run`-shaped call, not by regex; each test named so it can be checked individually.

**Nine tests use the actual trait method `Backend::run::<T>`, and all nine are in one file** —
`reverie-e9patch/tests/backend.rs`. All nine still exist at the pinned SHA, and the file contains
exactly nine occurrences of `Backend::run::<`, which corroborates the count independently:

1. `rewritten_syscall_supports_guest_injection`
2. `unsubscribed_rewritten_syscall_is_not_delivered_to_the_tool`
3. `rewritten_clone_restores_parent_and_child_contexts`
4. `rewritten_rt_sigreturn_uses_the_original_signal_frame`
5. `marker_collision_at_another_rip_is_not_a_syscall_event`
6. `exact_trap_with_unpatched_site_is_not_a_syscall_event`
7. `exact_trap_with_patched_site_documents_collision_filter_limit`
8. `pie_rewritten_site_validates_with_runtime_load_bias`
9. `non_elf_script_uses_ptrace_fallback`

Everything else that *looks* like a front-door call is an inherent wrapper — and these are the
files where the porting instruction would actually land:

| File | Calls | All of them |
|---|---|---|
| `reverie-liteinst/tests/hybrid.rs` | 21 | `run_host_with_*` variants |
| `reverie-examples/tests/e9patch_direct.rs` | 6 | `run_direct*` variants |
| `reverie-liteinst/tests/lifecycle.rs` | 5 | `run_with_*_preload` variants |

### 4.2 The wider corpus

**Confidence: MEDIUM.** These come from the pattern-matching pass described in §6.

Over **330 tracked non-vendored `.rs` files** (via `git ls-files`, excluding `vendor/` and
`third-party/`), there are **924 test functions**, split by location as **732 inline unit tests in
`src/` (79%)**, **124 integration tests in `tests/` (13%)**, and **68 elsewhere** (examples and
benches). Of those, roughly **236 execute a guest** — where "executes a guest" excludes bare
`Command::new`, which merely builds a command object and is ordinary scaffolding. **Exactly 2
compare two backends against each other.**

Bucketed by how they reach the runtime: **9 front door** (HIGH, §4.1), **~82 past the door** via
an inherent wrapper, backend construction, or a raw mechanism, and **~26 via an example CLI**
(`CARGO_BIN_EXE_*`) rather than the library.

**These buckets do not reconcile with the 236 total, and I am not going to paper over that.**
9 + 82 + 26 ≈ 117, roughly half. The residue is partly `safeptrace`'s 32 guest-executing tests —
deliberately excluded, since `safeptrace` does not depend on the `reverie` crate at all and its
tests are therefore not Reverie tests and cannot be ported anywhere — and partly tests the bucket
pass did not classify. **Treat 9 as a count and 82/26 as an order of magnitude.** If a decision
turns on the precise split, it needs per-file inspection like the four files in §4.1, not another
regex.

### 4.3 Per-backend guest-executing coverage — and why the fraction misleads

**Confidence: MEDIUM** (same pass). **Read the absolute column, not the fraction:**

| Backend | Guest-executing tests | Of total tests in crate | Fraction |
|---|---|---|---|
| **ptrace** | **36** | 217 | 17% |
| liteinst | 34 | 80 | 43% |
| e9patch | 29 | 82 | 35% |
| KVM | 4 | 222 | 2% |

**Ptrace has the highest absolute end-to-end coverage of any backend.** Its low percentage is a
**denominator artifact**: ptrace carries a large unit-test base, which inflates the denominator
without saying anything about its end-to-end coverage. Reading that 17% as a ranking inverts the
truth and puts the reference backend last.

**Ptrace is the leading edge.** It is the reference every parity claim should be measured
against, and it already has the most guest-executing tests. This document flags it explicitly
because the ratio was misread once already in this workstream.

---

## 5. The anti-pattern this is really about

**Confidence: HIGH.** Read directly at `reverie-kvm/tests/static_elf.rs:1654`.

```rust
fn static_elf_getppid_matches_ptrace_parity() {
```

**The name says parity. The test never runs ptrace.** Within the function body, "ptrace" appears
exactly three times — in the test name, in an explanatory comment ("The ptrace backend runs the
guest inside a real PID namespace…"), and in an assertion message ("must report `getppid()==1` to
match the ptrace namespace"). `PtraceBackend` is never constructed and never invoked. The test
builds a static-ELF guest, runs it under `KvmBackend`, and compares against the constants `1` and
`0` — **remembered ptrace values, hardcoded**.

The test is not worthless: it does check that KVM reproduces specific expected values, and it
fails loudly via `exit_group(42)` if KVM regresses. But **it cannot detect the failure its name
promises.** If ptrace's behaviour changed tomorrow, this test would keep passing while the parity
it claims to verify silently broke. It is a **frozen snapshot wearing the name of a comparison** —
the same absence-reads-as-success shape as a skipped CI job reported as green.

**This is the concrete cost of the trait gap.** When the front door cannot express a comparison,
comparisons get written as remembered constants instead — and a remembered constant is exactly the
artifact that stops tracking the thing it was copied from. Porting tests to a front door that
cannot express what they check does not merely fail to help; it manufactures more of these.

---

## 6. Method honesty: what the numbers are worth

The mechanical classifier behind every MEDIUM figure produced **four different answers** before it
was abandoned. Recording this because the reliability of a number is part of the number:

| Attempt | Result | Why it was wrong |
|---|---|---|
| 1 | 156 / 17 | counted bare `Command::new` as guest execution — inflated the population by ~100 |
| 2 | 146 / 5 | the "past the door" regex matched the backend **type name**, so `E9patchBackend::run::<T>` — which *is* the front door — was classified as bypassing it |
| 3 | 108 / 0 | per-test body splitting attributed helper functions to the preceding test, so one helper poisoned a whole file |
| 4 | 0 | the is-this-a-test lookback failed on the actual formatting |

Attempt 2 is worth dwelling on: it is exactly the error the structural criterion warns against —
**naming a backend is not bypassing the abstraction.** `E9patchBackend::run::<T>` names a backend
in the receiver and is nonetheless the purest front-door call in the tree.

The final front-door count was obtained by direct inspection of the only four files containing any
`Backend::run`-shaped call — a set small enough to be exact — and the nine tests are named so any
reader can check them individually. An earlier corpus figure of 783 was also wrong (two different
globs in play) and was corrected to 924; **do not use 783.**

---

## 7. The two options

Both are coherent. Both are argued here on their merits before any recommendation.

### Option (A) — grow the trait

Add a trait-level method, e.g. `run_with_output`, returning `(Output, GlobalState)`.

**In favour**

- **The signature is not a design question — it already exists three times, identically.** All
  three implementors independently converged on `Result<(Output, T::GlobalState), Error>` over one
  shared `Output` type (§3.1). This is promotion of a settled de-facto contract, not invention.
- **The reference backend already implements both halves as a pair**, one line apart. The trait
  currently exposes the weaker one. `Output` carries the exit status, so the richer method
  *subsumes* the current one rather than adding a parallel concept.
- **It makes the porting instruction satisfiable** for the population that needs stdout, which is
  the largest single category of wrapper use.
- **It directly serves the stated priority** — end-to-end testing of Reverie as a library through
  the front door. A front door that cannot observe what the guest printed cannot support most
  meaningful end-to-end assertions.
- **It attacks the §5 anti-pattern at the root.** Cross-backend comparison becomes expressible, so
  parity stops being written as remembered constants.

**Against**

- **It is a public API change to the abstraction users build against.** Every external
  implementor of `Backend` breaks unless the method ships with a default — and a default that
  discards output would reintroduce the silent-absence failure the change exists to remove.
- **It contradicts a stated design decision.** The trait doc explicitly commits to "the common
  denominator every backend shares" (§2.1). Changing that should be a deliberate revision, not a
  drive-by.
- **It invites scope creep toward preload, stats, and GDB.** §3.2 shows those are genuinely not
  uniform, so the boundary has to be defended explicitly or the trait accretes stubs.
- **Real implementation cost for ptrace**, whose variant lives on `TracerBuilder`/`Tracer`, not on
  `PtraceBackend`.

### Option (B) — accept them as backend-specific by design

Stop calling those tests portable. Scope the parity envelope to what the trait expresses.

**In favour**

- **It is already the documented intent** (§2.1), so it costs no API change, no migration, and no
  broken external implementors.
- **It is honest about mechanism differences.** Preload is meaningless for ptrace; stats exist in
  one backend. Pretending otherwise produces a trait whose methods most implementors stub.
- **It ends a false instruction immediately.** "Port these tests" stops being issued against a
  population that cannot satisfy it — which is a real gain today, at zero cost.
- **A smaller honest corpus beats a larger dishonest one.** Nine tests that genuinely swap
  receivers are worth more than ninety that compile and check nothing.

**Against**

- **It permanently caps the portable corpus at roughly the nine tests in one file** (§4.1) plus
  whatever is newly written. That is a very thin cross-backend assurance layer for a project whose
  stated direction is iterating on backend compatibility.
- **It leaves cross-backend comparison inexpressible through the front door**, so the §5
  anti-pattern remains the path of least resistance. Expect more remembered constants.
- **It concedes the common denominator is smaller than reality.** Every implementor *does* share
  output capture — accepting (B) wholesale means declaring a capability backend-specific that
  demonstrably is not.
- **It does not reduce maintenance.** The wrappers, and the per-backend tests on them, stay.

---

## 8. Recommendation

> *Everything above this line is neutral analysis. What follows is a position to react to.*

**Take (A), narrowly: add exactly one trait method, `run_with_output`, returning `(Output,
GlobalState)`. Take (B) for preload and stats, explicitly and in writing.**

This is a decomposition, not a compromise — the two halves of the gap have different evidence and
the evidence points in opposite directions:

1. **For output, (A) is close to free, and the usual objection does not apply.** The objection to
   growing a trait is that you must design an abstraction over implementations that differ.
   Here they do not differ: all three implementors already return the same type through the same
   shared `Output`, and the reference backend already exposes the with-output and
   discarding-output forms as a deliberate pair. **The abstraction has already been designed —
   three times, independently, in agreement.** Leaving it out of the trait does not avoid the
   design decision; it just means every caller re-derives it against a backend-specific name.

2. **For preload and stats, (B) is correct on the same evidence, read the other way.** Preload has
   no meaning for the reference backend and stats exist in one backend. Those are mechanism
   facts, not gaps. Putting them on the trait buys stubs, and a stub that returns `Ok` with
   nothing is the exact failure — capability silently absent, test green — that this whole
   exercise is trying to stop.

3. **Ship the method without a default implementation.** Requiring all three implementors to write
   it is a one-time cost across three files in one repo. A defaulted method that discards output
   would let a backend silently not implement capture while its tests pass — reintroducing
   absence-reads-as-success into the very method added to eliminate it. **If it breaks external
   implementors, that is the change being visible, which is the correct behaviour for a change to
   a stated contract.**

4. **Sequence it against the leading edge: ptrace first.** Ptrace has the highest absolute
   guest-executing count (§4.3) and is the reference for every parity claim, but its output
   capture is the one that is *not* already on the backend type (§3.1) — so it is simultaneously
   the most valuable and the most expensive of the three. Doing it first prices the whole change
   honestly instead of discovering the cost after two easy backends.

5. **Then retire the §5 anti-pattern as the first use.** Once comparison is expressible,
   `static_elf_getppid_matches_ptrace_parity` should either run both backends or be renamed to say
   what it actually does. It cannot be ported today — KVM does not implement the trait, so there
   is no receiver to swap to — which is worth knowing before anyone puts it on a porting list.

**What I would not do:** issue "port tests to the front door" as a general instruction against
today's trait. For the ~82 past-the-door tests it is unsatisfiable, and an agent told to satisfy
it will satisfy it — by dropping the stdout assertion. That trades a backend-specific test that
checks something for a portable test that checks nothing, and the result reports green either way.

**What would change my recommendation:** if the three `Output` signatures turn out to differ in
some way this reading missed, (A)'s central argument weakens sharply and (B) becomes right for
output too. That is a HIGH-confidence claim read from three files (§3.1) and it is the single
load-bearing fact under this recommendation — **it is the one thing worth re-checking before
acting on this document.**

---

## Appendix: reproducing every HIGH-confidence claim

At Reverie `f7e02e918570392426eb1a3e6a7978571db6c217`:

```bash
# §2 — the trait declares exactly one method
sed -n '147,167p' reverie/src/backend.rs

# §2.2 — the three implementors (excludes vendor/ and two doc examples in reverie/src/)
grep -rn "impl Backend for" --include=*.rs . | grep -v vendor

# §3.1 — the converged Output signature
grep -A6 "pub async fn run_with_output<T>"             reverie-e9patch/src/backend.rs  | grep "Result<"
grep -A6 "pub async fn run_with_output_and_preload<T>" reverie-liteinst/src/backend.rs | grep "Result<"
sed -n '1402p;1446p' reverie-ptrace/src/tracer.rs
grep -n "pub use child::Output" reverie-process/src/lib.rs

# §4.1 — nine front-door tests, nine trait calls, one file
grep -c "Backend::run::<" reverie-e9patch/tests/backend.rs

# §5 — the anti-pattern names ptrace and never runs it
sed -n '1654,1730p' reverie-kvm/tests/static_elf.rs | grep -in "ptrace\|PtraceBackend"
```
