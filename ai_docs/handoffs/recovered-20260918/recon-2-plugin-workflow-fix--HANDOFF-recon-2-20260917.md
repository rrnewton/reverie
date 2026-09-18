# recon-2 handoff — 2026-09-17

Facts are in the TaskGraph notes and on the pull requests. This is the reading
of them, which is the part that dies with me.

Status file `/home/newton/temp/recon-2-status.md` is the live index. The notes
on `four-named-assertions-across-four-backends-are-what-keeps-main-red` carry
the evidence for everything below.

---

## The four red assertions: what I believe each one actually is

**Two of four I consider understood, two I do not.**

### DBT — UNDERSTOOD. Not a hermit defect.

`tests::dbt_public_dispatch_runs_echo_through_detcore` and its `_status_`
sibling fail together with one mechanism. The cause is a DynamoRIO client
assertion in **reverie**:

    reverie-dbt/native/client.c:3995: DR_ASSERT(evidence_callback_depth == 0)

`evidence_callback_enter()` / `evidence_callback_leave()` bracket
`reverie_dbt_runtime_thread_init`, and the depth is non-zero afterwards. A
`DR_ASSERT` abort is why the process exits 255 having completed zero syscalls
with an empty thread tree.

What I believe, beyond what is proven: the imbalance most likely comes from the
Tool re-entering during initialization. `reverie_dbt_runtime_thread_init` is
hermit's `detcore-dbt` entry point, and `detcore-sabre` carries a comment about
exactly this hazard — "Adapter construction may itself call libc getrandom, so
recursive calls…". The sibling backend already knows the pattern. **I did not
prove this** and the alternative — that nesting is legitimate here and the
assertion is simply too strong — is equally consistent with what I measured.
Whoever takes it should decide that question first, because the two have
opposite fixes.

**It is environment-specific and that is solid.** Same source blob, same reverie
pin, the failing run's own staged DynamoRIO: passes on the host with 167
syscalls, aborts in the hermetic container. So the dispatch path, the
private-loader fix, and the test are all exonerated. Do not go looking in
hermit's DBT code.

### SaBRe — NOT understood, but much narrower than it was.

The product claim is precise and worth restating because my first framing was
wrong: this is **not two backends disagreeing on a determinized value**. ptrace
returns a byte-identical digest across runs six hours and three reverie pins
apart; SaBRe returns a different one every run. **SaBRe is not determinizing
`getrandom` at all** — the guest gets fresh entropy. In a determinism product
that is the more serious of my two.

Four candidate causes are excluded by measurement: the bootstrap-random bypass
(needs buflen 32 + `GRND_NONBLOCK`, guest calls `(16, 0)`), the vDSO (glibc
2.34, `vgetrandom` arrived in 2.41, so a real syscall instruction is issued),
a general interception failure, and — the one I was most confident in —
reverie's documented coverage limits.

**That last exclusion is the most useful thing I learned about SaBRe.** I was
pointing at `.text`-only scanning, libraries recognised by name, no hook for
arbitrary DSOs. Every one of those produces a *broad* gap. Hermit has a
fail-closed check that fires on total interception failure — "no syscall was
intercepted, so this run applied no determinism" — and in the failing run that
message is absent and five of seven sibling sabre tests pass. A broad gap would
have tripped it. So the defect is narrow enough to let five parity tests through
while `getrandom` alone leaks. **Do not restart on the coverage-gap theory; it
is already refuted.**

### KVM and LiteInst — not mine, and one resolved to a defective test

Claimed by their own backend lanes. LiteInst resolved to a defective test rather
than a product defect — native, ptrace and LiteInst behave identically and the
test expected a supplementary group the image lacks. Carry that as the reason to
ask of every one of these "is the assertion right?" before assuming the backend
is wrong. It is the only one of the four that turned out that way, and I checked
both of mine against it: both assertions are correct.

---

## Why 3058 turned out to be critical-path

`https://github.com/rrnewton/hermit/pull/3058` adds the guest's stderr to three
backend status assertions. One file, no behaviour change, and it looks like
cleanup.

I told the coordinator it was critical path, then corrected myself to say it was
not — because `hermit run --backend dbt` inside the image would show the same
stderr with no code change. **That correction was wrong in practice.** What
actually produced the DBT root cause was 3058's message applied inside the
container. Before the message existed, four separate readings of the retained
logs could not get past "Exited(255), 0 syscalls". After it existed, the
diagnosis took one run.

The general reading: an assertion that discards information already in its own
scope is not a style problem, it is the thing standing between a red node and
its cause. `assert_eq!(output.status, …)` with `output.stderr` two fields away
and unused is the specific shape. Three sites had it; a fourth
(`dbt_public_status_…`) genuinely has no stderr to show because it calls
`run_with_backend`, and I left it alone deliberately rather than adding a
message that restates the numbers.

3058 is gate-CLEAR, mergeable, **zero approvals**, and touches a currently-red
node — so it needs a non-author review and a judgement on the red-node
constraint. I think the exception is defensible: a change that only adds text to
a failure message cannot hide a regression, which is what that constraint exists
to prevent. I did not take the exception myself.

---

## What the container recipe makes newly feasible

`ci/hermetic/run-in-pinned-root.sh` runs any command inside the pinned podman
image validate uses. I said instrumented SaBRe runs were "clearly feasible" and
then did not finish one — here is exactly what that unlocks and what stopped me.

It reproduced DBT **exactly** — same exit, same zero-syscall shape — which is
what turned DBT from "fails in CI" into a one-run diagnosis. The same is
available for SaBRe: run the guest under SaBRe in the image and read whether a
`getrandom` ever reaches the tool. That single observation decides between "the
syscall is never intercepted" and "it is intercepted and mishandled", which are
the two remaining families and have nothing in common.

**What blocked me was staging, not understanding.** SaBRe additionally needs the
loader *with its shared libraries* resolvable inside the image; copying the bare
loader out of `install_pkg` fails on `libelf.so.1`. Stage the whole `rsrcs` tree,
or drive the node the way validate does instead of hand-assembling it.

Two traps in the recipe, each of which cost me a run:

- **`target/` is a separate volume.** Anything built or staged there vanishes
  between invocations, and anything placed under the host worktree's `target/`
  is invisible inside. Build and use in **one** invocation; stage inputs outside
  `target/`.
- **The cargo home must already hold the exact reverie pin.** There is no
  network by design, so an older cargo home dies with "can't checkout … offline
  mode". The one named in the failing run's own handle had already been swept.

And a trap that is not about the container: **a backend test that skips reports
as a pass.** `if Backend::Dbt.ensure_available().is_err() { return; }` produces
"1 passed" in 0.00s. The only visible difference from a real pass is the
duration. Any backend suite run without the runtime staged is green and means
nothing — I nearly filed such a green as a result.

---

## The out-of-scope reporter's condition, and why the monitor must be the paths

`RETIRES-SCOPE` (dev-hermit#368) lets a retirement survive a head move that
leaves its declared paths byte-identical. `paths_changed_outside_scope`
(dev-hermit#369) reports what else moved. Both are landed. **The mechanism is
armed, not dormant** — `land-pr.sh:916-917` already passes `--source`, which I
initially and wrongly reported as absent.

The coordinator proposed monitoring for "a scope declared **narrower than the
objection's subject**". That monitor does not protect against the real failure,
and the distinction needs to survive.

The real failure is a scope that **correctly covers** the objection's subject
while the objection **depends** on something outside it. An objection that
`demos/x.sh` misuses a helper, scoped to `demos/`, revived when the helper
changes under `ci/`. The declared paths are genuinely unchanged; the retirement
survives *correctly by its own rule*; the objection is live again. There is
nothing wrong with the scope, so a human looking for an over-narrow scope sees
nothing to flag.

That is why the monitor has to be **the reported out-of-scope paths**, not a
judgement about scope width. The report is produced automatically at the moment
the retirement is honoured; the judgement requires someone to already suspect
the problem. And `paths_changed_outside_scope` is deliberately **observability,
not a check** — it cannot decide whether the objection depended on what moved,
because that needs to know what the objection was about, and the objection is
prose. An unjudged risk that is reported beats an unjudged risk that is silent.
Do not let anyone "upgrade" it into a check; it would have to guess.

One more thing to hold: the coordinator's original justification — "the declarer
already holds retire authority, so no new power is granted" — is **wrong**, and
they accepted that. Today's retirement is a judgement about one head the
declarer examined; a scoped retirement is a judgement about future heads they
have not seen. That is new power. Bounded and reversible, but new, and the
decision rests on the cost of fifteen rebases rather than on no-new-power.

**Approvals have the identical defect and #368 does not fix them** —
`comment_shas[-1] == head`, a plain sha equality. I deliberately did not extend
the mechanism to approvals: a retirement scoped to `demos/` makes a narrower
claim of the same kind, but an approval scoped to `demos/` surviving a change in
`ci/` would approve code nobody read. Blast radius is the whole diff instead of
one objection. **Budget a fresh review per rebase in any drain plan.**

---

## The one habit worth inheriting

Almost every error I made tonight, and several I caught in others' work, was the
same shape: **an instrument that returns a confident answer about the wrong
thing.** A glob that overflows to zero. A per-line grep against a two-line shell
continuation. A regex whose `[^\n]*` eats the continuation backslash. A loader
that reports only passing rows. A failing-node count that shrank because the run
died earlier. A test that skips and says "passed". A pin that moved under three
turns of analysis.

None of these errored. Each produced a number that read as information. The only
defence that worked repeatedly was checking a result against something I already
knew independently — a zero that contradicted a failure I had read with my own
eyes, a count that contradicted a depth. Cheap, and it caught every one.
