# Owner directives on CI and validate: ranked ledger and measured gap

**Transient report.** 2026-08-10, `claude-reviewer-2`. Answers the owner's request to extract his
prompts from this session **and its predecessor**, rank the CI/validate directives most-recent-first,
and measure the gap.

---

## How the corpus was identified, because getting this wrong invalidates everything

Owner prompts are **not** in `~/.tg/hermit.db` / `hermit2.db` — `agent_logs` there is **empty (0
rows)** — nor in `~/.orc/index.db`, which holds session metadata only. They are in
`~/.orc/sessions/<id>/session.db`, table `content_blocks`.

**`role='user'` is not the owner.** The predecessor session has 21,341 `role='user'` blocks and
**18,675 of them have `user_source IS NULL`** — system wakeups like *"2 unassigned task(s) ready"*
and *"Agents are idle and need direction"*. Taking `role='user'` at face value would have inflated
the corpus ~8× with the fleet's own machinery and attributed it to him. I printed a sample of every
`user_source` class and read them before filtering.

**Owner** = `role='user' AND user_source IS NOT NULL`, excluding `{"Orc":…}` (another ORC session
relaying). Sources seen: `Submitted/Web`, `Submitted/Tui`, `GChat` with `is_owner:true`.

| session | role | owner prompts | span |
|---|---|---|---|
| `4fb50e87` "hermit" | **predecessor** | **2,662** | 2026-07-20 → 2026-08-08 04:55:32 |
| `1b176094` | current | **113** | 2026-08-08 04:58 → 2026-08-10 05:01 |

Three other `dev-hermit` sessions exist and are empty or near-empty (0, 0, 1). **Total corpus
2,775.** The predecessor is **96%** of it — which is exactly why its directives are inherited
rather than void.

---

## The bar, verbatim

> *"That means PRs drained and a very solid CI where I can drop in and run validate in <10min with
> clear output that makes sense."* — 2026-08-10 05:00:40

---

## Ranked ledger — most recent takes precedence

### Current session (highest precedence)

| # | when | directive (verbatim core) | status | evidence |
|---|---|---|---|---|
| **D1** | 08-10 05:00 | *"PRs drained and a very solid CI where I can drop in and run validate in <10min with clear output that makes sense"* | **PARTIALLY MET** | see the three sub-measures below |
| **D2** | 08-10 05:00 | *"extract … all my prompts … make a list of the directives … measure the gap … keep fixing it"* | **MET** (this document) | 2,775 prompts extracted from both sessions |
| **D3** | 08-09 22:47 | *"general termination … record logs … know exactly which test within a test suite in a given step was the culprit. Minutes not hours"* | **PARTIALLY MET** | runner-termination is wiring a run timeout that names the overrunning row; not landed |
| **D4** | 08-09 22:44 | *"validate itself should have a timeout. Both in the inner c group steps and the outer c group level"* + *"the reviewer should not allow through anything that requires a massive jump to the validate timeout cap"* | **PARTIALLY MET** | reviewer-brief half is in flight and I have been enforcing it (I refused cap raises on #1864 and recorded it); the timeout half is **NOT MET** — `.github/ci` contain **zero** `HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS` uses, so hosted validate is unbounded |
| **D5** | 08-09 22:34 | *"ruining validate is never allowed under any conditions. A 7X slowdown…"* | **UNVERIFIED BY ME** | I did not reproduce the 7× repair; it is another agent's measurement and I am not laundering it into a verified fact |
| **D6** | 08-09 22:52 | scripts calling the `tg` CLI are dangerous | **NOT MEASURED HERE** | outside CI/validate scope |

### Predecessor (inherited, applies unless superseded)

| # | when | directive | status | evidence |
|---|---|---|---|---|
| **D7** | 08-08 04:15–04:28 | validate Rust port — *"I DONT see the validate ported to Rust!?!?!?"*, *"port over those 111 rows"* | **PARTIALLY MET** | `validate.rs` exists; the owner's own check was that `validate.sh` was still on main |
| **D8** | 08-08 00:41 | *"Keep driving cleanliness on Main and validation … stale branches cleaned up"* | **NOT MET** | 188 open PRs; branch sprawl unaddressed |
| **D9** | 08-07 23:53 | *"HOW LONG did the (fixed) validate.sh take in wall time"* — always report wall time | **MET** | wall time is now reported on every validate receipt and in the tick |
| **D10** | 08-07 23:47 | *"Fixing red main is always P0 and lands directly on main"* | **STANDING, ACTIVE NOW** | hermit main is red as of this writing |

---

## Contradictions and reversals — flagged, not silently resolved

**R1 — "do not merge" was LIFTED.** Predecessor 2026-07-23 14:21:37, responding to *"Agent
correctly did NOT merge them"*:

> *"Uh... I guess you completely forgot that we are in post-facto review mode as documented in the
> skill."*

Not merging was the **error**. Any agent still defaulting to "do not merge" today is following a
superseded directive.

**R2 — the DBT rename was originally GATED, then ungated.** Predecessor 2026-07-30 13:14:47:

> *"I've been meaning to file a task gated on a future state where we have zero open PRS and can do
> a big rename … call the backend dbt instead of dbi."*

Later reversed to land-now (per the dispatch). The gate condition — zero open PRs — has **not** been
met either way: 188 are open.

**R3 — the pin gate moved from exact-equality to ancestry + monotonicity** (per the dispatch;
I did not re-derive the original prompt).

**Standing tension worth naming:** D10 says red main is P0 and lands **directly on main**, while the
PR/review protocol requires a PR plus dual review. `AGENTS.md`'s *Red-Tip Direct Land* reconciles
them, but only for *the fix for that red* — an agent reading either rule alone will act wrongly in
good faith.

---

## The gap, measured — both terms throughout

**Sub-measure 1 — "PRs drained".** **NOT MET.**
`188 total open` / `23 non-draft (ready)` / `165 draft`. The circulating figure "~25 PRs still open"
matches the **ready** count only; the total is 188. Which denominator "drained" means is the
owner's call, and the two differ by 7.5×.

**Sub-measure 2 — "run validate in <10min".** **UNVERIFIED BY ME.** The claimed measurements are
8m40s PASS and 7m20s FAIL on the full 187-program corpus — under ten minutes twice, but one of two
**failed**, and both were clean-admission rather than the quiet-box condition the owner describes
("drop in and run"). I did not reproduce either run and will not restate them as verified.

**Sub-measure 3 — "clear output that makes sense".** **PARTIALLY MET, and measurable.** Seven
defects found across eleven lines of output; **six fixed**. The three named symptoms — bare `make`
printing **no completion verdict at all**, three gates **silent on success**, and a submodule
verdict conveyed by **porcelain whitespace** — are the concrete form of this directive, and treating
it as a nicety is how they survived.

**Main state.** **RED**, verified independently rather than accepted: `github_main_health` reports
`rrnewton/hermit main=2b38d8e629ee state=red`, caused by `CI (GitHub-managed portable)`
**SELF-TIMEOUT / completed-cancelled**. `dev-hermit` main is `pending` (a hole, not a red) and
`reverie` is green. The hermit red is the hosted stall — my earlier analysis' **bucket 2**, an
external/self kill of an **unbounded** run, whose remedy is exactly D4's missing timeout.

---

## The single highest-leverage gap

**D4's timeout half is the root of the current red.** Hosted validate is unbounded
(`zero HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS uses in .github/ci`), so it can be killed externally
before reporting; that kill is what makes main red right now, and because the run dies without
naming a row, D3's *"know exactly which test … was the culprit"* is also unmet by the same absence.
One fix discharges two directives and unblocks the red that blocks D10.

## What I did not do

Did not re-measure validate wall time, did not verify the 7× repair, did not touch a PR, label,
branch or workflow. This is extraction and measurement only.
