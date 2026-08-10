# Worktree discipline: audit against the owner's procedure, and what would force compliance

**Transient report.** Written 2026-08-09 by `claude-reviewer-2` at owner request.
Audits the prompt/skill/AGENTS surface of `~/work/dev-hermit` against the seven-step
worktree procedure the owner stated, classifies every gap, and names a forcing function
for each recommendation.

Every claim below cites a path, and a line number where the text is the finding.

---

## The one-paragraph version

**The mechanism the owner asked for already exists and is used by nothing.**
`orc.spawnCliAgent` has accepted a `cwd` option all along
(`~/.orc/orc.d.ts:477`), and `orc.listAgents()` already *returns* each agent's `cwd`
(`~/.orc/orc.d.ts:447`). No call site in this repository passes `cwd`, and no check reads
it. So the enforcement point and its detector are both present and both unwired. Tonight's
17-of-18 shared checkout was not a missing capability; it was an unused one.

Underneath that sit two structural faults. **Procedure step 1 is impossible for the actor
it names** — the ORC coordinator has no shell and no registered script that can allocate a
worktree — so the policy discharges it by *asking an agent*, which is the failing mechanism
the owner already called out. And **step 7 is enforced only on the release path nobody
takes**: the orphan refusal is gated behind `--clean` and bypassable with `--force`, while
the default path merely warns and leaves the worktree on disk. That one default is
simultaneously the orphan hole and the accumulation engine.

Finally, the policy file does not merely fail to prohibit the shared checkout — **it
blesses it in writing** (`AGENTS.md:53-54`).

Counts: **5 contradicted, 5 not documented, 6 documented-but-unenforced.**

---

## Scope, method, and what I did not verify

Audited: `AGENTS.md` (44 KB; `CLAUDE.md` is a symlink to it), `ai_docs/agents-md-policy-rationale.md`,
`.llms/skills/` (all), `agent-utils/skills/`, `.orc/plugins/hermit-dev/`,
`scripts/allocate-worktree.rs`, `scripts/release-worktree.rs`, `ci-hub/health/`,
`ci-hub/health/tick-hub.yaml`, and `~/.orc/orc.d.ts`.

**Inherited, not verified by me:** the census figures (313 registered worktrees across 5
repos, 182 unregistered clones, 53 locations with unpushed commits, 38 with uncommitted
edits, 17-of-18 agents sharing the parent, a `--checkout` policy reverting a submodule pin).
These come from the dispatch. I mark them inherited throughout and none of my findings
depends on them — each is independently grounded in a cited file. Where the numbers matter
is only in ranking.

**Not audited:** individual dispatch prompt texts. They are not in the repository, so I
cannot cite them; the owner has already characterised them, and see §C-meta.

---

## Bucket A — CONTRADICTED

*A file says something incompatible with the procedure. Fix: correct the text.*

### A1. The policy explicitly blesses the shared checkout — this is the headline

`AGENTS.md:49-63`, section **Parent Working Tree**:

> `AGENTS.md:53-54` — "Reading the parent is always fine, and the old 'one agent,
> occasionally' rule was unenforceable and universally ignored — **15+ agents sit here
> right now.**"

The policy observed the exact condition the owner now identifies as root cause, and
responded by *relaxing the rule to match it*. It then narrows the hazard to writes only
(`:53`, "The hazard is WRITES, not presence"), which is why nothing treats 17 agents in the
shared tree as a defect: **under current policy it is not one.**

`AGENTS.md:60-63` then authorises the writes themselves:

> "write to the parent only under a task that names the parent paths it owns … **A registry
> records ownership and cannot enforce it.**"

Against **procedure rule 3** ("the `dev-hermit` repo belongs to the coordinator and is
modified only by it"), this is a direct contradiction: any agent may write any parent path
if its task names it. The final clause is the policy diagnosing its own unenforceability
and shipping anyway.

`AGENTS.md:51` compounds it: "`~/work/dev-hermit` is not a workspace, and **neither the
invariant above nor Hard Invariant 1 binds it**" — explicitly exempting the parent from the
no-feature-work-in-a-primary rule.

I am not arguing this section was written carelessly. It documents real incidents and its
write-hygiene rules are sound. The defect is that it treats *presence* as harmless, and
presence is precisely what produced a torn tree tonight.

### A2. The caps count worktrees and agents separately

`AGENTS.md:106` (Hard Invariant 13):

> "Never exceed **twelve active worktrees, five parked slots, or fifteen agents** (count
> each separately; active work does not consume the parked allowance)."

**Rule 6** requires `worktrees/` to *precisely match* active agents. HI-13 permits 17
worktrees against 15 agents and instructs that they be counted independently. A parity rule
and a three-independent-budgets rule cannot both hold.

### A3. "Parked" is a sanctioned agentless worktree

`AGENTS.md:82` defines it: "**parked** means clean, detached, cache-retained, and omitted
from `ACTIVE.md`". `AGENTS.md:131-133` operationalises a standing pool: "At five parked
slots, reclaim before creating another."

A parked slot is by construction a worktree with no agent, deliberately invisible in the
registry. That contradicts **rule 6** (parity) and **rule 5** (short-lived: land, then next
piece or clean up). Parking is the sanctioned third option the procedure does not have.

### A4. The parent is documented as the home of *durable* `ai_docs/`

`AGENTS.md:285`:

> "The parent owns policy, gitlinks/ignores, `ARCHIVED.md`, generic orchestration, **durable
> `ai_docs/`**, and reproducible `experiments/`"

**Rule 2** says the opposite: a parent-level `ai_docs/` file is permitted *when it is a
transient report*, and durable reference docs belong in the inner `hermit/` repo and are
rare. The current corpus follows `AGENTS.md`, not the procedure.

### A5. Provisioning is described as coordinator-delegated, not coordinator-performed

`AGENTS.md:138-139`:

> "the **coordinator delegates that run** and confirms the registered row before dispatching
> into the slot."

**Rule 1** says the coordinator manages worktrees using scripts. The text substitutes
delegation, because of the capability gap in §C2. This is a contradiction in the weak sense
that it redefines the duty rather than fulfilling it — and it is the seam through which
agents began provisioning their own slots.

---

## Bucket B — NOT DOCUMENTED

*Silence where the rule should be stated. Fix: add it.*

### B1. Rule 2 (spawn with CWD inside the slot) appears once, in the wrong voice

A corpus-wide search for any statement of the CWD rule returns exactly one line:

> `AGENTS.md:153` — "Run all work from the assigned child with **explicit cwd**."

Three problems. It is a subordinate clause at the end of a dense paragraph in *Strict Slot
Pool → Start*. It addresses the **agent** ("run all work from…"), asking it to remember a
`cwd` argument on every command, rather than the **spawner**, which could set the process
working directory once and make the shared tree unreachable. And no skill repeats it —
`.llms/skills/hermit-coord/SKILL.md`, the co-coordinator role that dispatches, never
mentions `cwd` at all.

This single clause is the pre-existing instance of the pattern the owner flagged: the rule
was written as a request to the actor who must not violate it, instead of a property of the
process.

### B2. The "two write locations" are never enumerated

Rule 2 grants exactly two parent write targets: an assigned `experiments/foo/` subdirectory
and a transient parent `ai_docs/` file. No file states this. `AGENTS.md:60` instead gives
the open-ended "paths the task names", and `AGENTS.md:283-289` describes what the parent
*owns* without ever saying what an agent may *write*. An agent reading the corpus cannot
derive the two-location rule.

### B3. Rule 3 is absent, not merely contradicted

No file anywhere states that `dev-hermit` belongs to the coordinator. §A1 is the closest
text and it says the opposite.

### B4. Rule 5's lifecycle is undocumented

There is no statement of work → PR → green → land → (next piece in the same slot | retire
slot and agent together). `AGENTS.md:129-139` and the *Close/park/reclaim* text describe
parking and reclamation, i.e. how a slot **persists**, never how it **retires**. The
procedure's terminal state has no written form.

### B5. Transient vs durable `ai_docs/` is never defined

Rule 2 turns on that distinction. No file defines it. Empirically the corpus splits
`ai_docs/*.md` (dated, flat) from `ai_docs/transient/*.md`, and both contain material that
looks transient — this report's own placement was a judgement call, not a lookup.

---

## Bucket C — DOCUMENTED BUT UNENFORCED

*The important bucket. Fix: a mechanism, never a sentence.*

### C1. The `cwd` parameter exists and nothing passes it

```
~/.orc/orc.d.ts:477  spawnCliAgent(name, tab?, opts?: { cli?, flavor?, args?,
                     permissions?, role?, cwd?: string, remote?, workspaceId?, ... })
~/.orc/orc.d.ts:483  spawnOrcAgent(name, tab, opts?: { remote?, cwd?: string, ... })
~/.orc/orc.d.ts:447  listAgents(): Promise<[{ name, ..., cwd?: string, ... }]>
```

A repository-wide grep for `spawnCliAgent`/`spawnOrcAgent` across `*.ts`, `*.js`, `*.mjs`,
`*.md` finds **no call site at all** — only prose references inside
`ai_docs/2026-08-06-orc-spawn-conflates-cli-type-with-model-selection.md:145,147,154,242,286`.
Spawning happens through a path that does not set `cwd`, so every agent inherits the
coordinator's working directory, which is the shared parent. That is the direct mechanical
cause of 17-of-18, independent of what any prompt said.

The detector is equally idle: `listAgents()` already reports each agent's `cwd`, and nothing
in `ci-hub/health/` or `tick-hub.yaml` reads it.

### C2. The coordinator cannot allocate a worktree

`AGENTS.md:29-34` states the constraint plainly: "**The ORC coordinator HAS NO SHELL** — its
only effects are `orc.*` plus scripts pre-registered via `orc.registerScript`."

`.orc/plugins/hermit-dev/index.ts:739-762` registers exactly four scripts:
`hermitOperationalTick` (`:739`), `hermitSpeculativeLandObligations` (`:745`),
`hermitVerifyAgentDeath` (`:751`), `hermitSpeculativeLandWakeSent` (`:759`).
**None allocates, releases, or inspects a worktree.**

So rule 1 is not under-enforced; it is *uninstantiable*. The policy's own workaround
(`AGENTS.md:138-139`, "delegates that run") converts a coordinator duty into an agent
request. When agents later provisioned slots themselves to escape a torn tree, they were
following the only path the tooling leaves open.

### C3. Orphan protection is gated behind the non-default flag

`scripts/release-worktree.rs`:

- `:8-9` — "inspects each product worktree for uncommitted changes and **warns loudly**.
  `--clean` physically removes the worktrees … **but only when they are clean** or
  `--force` is given."
- `:47-48` — `--clean` … "Refused if a worktree has uncommitted work OR unpushed commits"
- `:65` — `--force` … "Allow `--clean` despite uncommitted/unpushed work (dangerous)."
- `:68-69` — "**Without `--clean` the physical worktree is left in place** (cache retained)
  and the slot is marked released in state."

**Rule 7** ("releasing must never orphan work") is therefore enforced on exactly one path,
and that path is opt-in and overridable. The default release warns, deregisters, and leaves
the tree on disk. I exercised this default twice today (slots `rev1864`, `rev-epoll`) and it
is the correct choice for cache retention — which is the point: the safe-feeling default is
the one with no orphan gate, and it is also what accumulates worktrees.

### C4. Nothing checks worktree/agent parity

`ci-hub/health/tick-hub.yaml` wires 15 checks. The only worktree one is
`worktree_disk_residue` (`:90`), which measures **disk reclaimability**, not parity:

```yaml
:90   - name: worktree_disk_residue
:93       cmd: python3 ci-hub/health/slot_disk_residue.py --gate
:97       skill: hard-warn-coordinator
:101      title: "worktree slots are idle on disk and reclaimable: {summary}"
```

Two defects. Rule 6 is a **parity** predicate (`|worktrees| == |active agents|`) and nothing
computes it. And the emission is `hard-warn-coordinator` — an alarm addressed to an actor
that `AGENTS.md:29-34` says has no shell and, per §C2, no registered script to act on it.
That is an unsatisfiable alarm: the only available response is to ask an agent, which is the
failing mechanism.

### C5. The unpushed-work detector cannot see inside slots

`tick-hub.yaml:195,198` runs `./ci-hub/health/unpushed_parent_commits.py --scope all`.
Despite `--scope all`, the scope is **all refs of the parent repository**, not all
repositories: `unpushed_parent_commits.py:80` builds `["--all"]` as a `rev-list` argument,
and `:74` runs `git -C <cwd>` against the parent. Its own docstring confirms the framing —
`:41-43`, "Cost does NOT scale with worktree count. 47 worktrees: `rev-list --all` 0.13s".

The inherited census puts 53 locations holding unpushed commits. Those live in the
`hermit/`/`reverie/` child repos inside slots. **This check cannot see any of them.** The
one detector aimed at rule 7 is pointed at the wrong repositories.

### C6. Dead-owner detection exists, is correct, and is fail-closed by design

`ci-hub/health/holder_liveness.py` and `ci-hub/health/residue_sweep.py` do the hard part
already: `holder_liveness.py:11` classifies a holder by ORC-fleet membership *and* whether
any same-user process has a cwd below the slot; `residue_sweep.py:225-238` enumerates
`/proc` cwds for exactly that.

`holder_liveness.py:4-8` records the scale: "`worktree-state.json` holds 96 slot records
whose … owner's ACTIVE slot is indistinguishable from a live one." And `:18-22` records why
detection does not become action: "**RELEASE IS FAIL-CLOSED, AND THAT IS NOT THEORETICAL.**
Of the 60 slots the sweep [identified] …" — refusals follow Hard Invariant 14 (never remove
a dirty slot without a documented recovery SHA).

This is the correct safety posture and I am not proposing to weaken it. But the loop is
open: detection produces refusals, and **no mechanism routes a refusal to a rescue**. Slots
accumulate in a state that is correctly diagnosed, correctly refused, and never resolved.

### C-meta. The prompt-text mitigation is this same bucket

The owner already identified it: writing "use a pinned worktree" into individual dispatch
prompts is the same class of control as `AGENTS.md:153`. Both put the invariant in text
addressed to the actor who must not break it. `AGENTS.md:53-54` is the empirical verdict on
that class — "unenforceable and universally ignored" — written about the previous rule, by
the same corpus, before tonight.

---

## Recommendations, prioritised, each with its forcing function

Ordered by (blast radius prevented) / (effort). For each: the mechanism, why it *forces*
rather than requests, and how to bracket it. Per the repository's own gate-design rule,
each needs a planted negative *and* a planted positive before it is believed.

### P0-1 — Set `cwd` at spawn, and refuse a spawn without one

**Mechanism.** A spawn wrapper that requires `cwd`, resolves it, and refuses unless it is
under `worktrees/<slot>/`. Pass it to `orc.spawnCliAgent` (`~/.orc/orc.d.ts:477`).

**Why it forces.** The agent's process working directory *is* its slot. Relative paths
cannot reach the shared parent; a bare `git status` reports the slot. No prompt sentence is
required and none can be forgotten — this is the owner's own example, and the parameter is
already there.

**Bracket.** Negative: spawn with no `cwd`, and with a `cwd` outside `worktrees/` → both
refused. Positive: spawn with a valid slot → agent's `/proc/<pid>/cwd` resolves inside it,
confirming the wrapper fires rather than being inert.

**Caveat, stated rather than glossed.** This confines *relative* access only; an agent can
still write an absolute path into the parent. That is what P0-3 and P1-3 cover. It also does
not by itself satisfy rule 2's two write locations.

### P0-2 — Register slot allocation and release on the plugin surface

**Mechanism.** `orc.registerScript("hermitAllocateSlot", …)` and `hermitReleaseSlot`
alongside the existing four in `.orc/plugins/hermit-dev/index.ts:739-762`, wrapping
`scripts/allocate-worktree.rs` and `scripts/release-worktree.rs`.

**Why it forces.** It makes rule 1 *possible*. Today the coordinator physically cannot
allocate, so "delegate it" is not laxity but the only option, and every delegation is a
place discipline can be dropped. `AGENTS.md:29-34` already prescribes this exact remedy:
"To make a command routinely coordinator-runnable, register it on the plugin surface."

**Bracket.** Positive: coordinator allocates with no agent involved and the registered row
appears. Negative: the script refuses a path shape outside `worktrees/<slot>/`.

### P0-3 — Tick check: every agent's cwd must be its own slot

**Mechanism.** A `tick-hub.yaml` check reading `orc.listAgents()` (`~/.orc/orc.d.ts:447`,
which already returns `cwd`) and failing on any agent whose `cwd` is not under its
registered slot. Cross-check against `/proc`, which `residue_sweep.py:225-238` already
implements.

**Why it forces.** It converts the invariant from unobservable to measured, and it is the
regression test for P0-1 — without it, one spawn path that forgets `cwd` silently restores
tonight. It is also nearly free: both data sources exist.

**Bracket.** Plant one agent in the shared parent → check fails naming it. All agents in
slots → passes with a nonzero count of agents actually examined (a zero-agent pass is the
failure mode to guard).

**Route it to an actor with hands.** Do not emit `hard-warn-coordinator` (see §C4). Emit to
`hermit-coord`, which per rule 4 and `.llms/skills/hermit-coord/SKILL.md:141-144` has both
the shell and the duty.

### P1-1 — Move the orphan check out from under `--clean`

**Mechanism.** In `scripts/release-worktree.rs`, run the dirty/unpushed/no-PR check on
**every** release path, not only `--clean` (`:47-48`). Default release should refuse to
deregister a slot holding unpushed commits or uncommitted edits; retaining the worktree on
disk stays fine.

**Why it forces.** Rule 7's guarantee currently depends on choosing the flag that triggers
it. Under the fix, the cheapest path is also the safe one, and orphaning requires an
explicit `--force` that is already documented as dangerous (`:65`).

**Bracket.** Negative: plain release of a slot with an unpushed commit → refused (today:
warns and proceeds). Positive: plain release of a clean, pushed slot → succeeds, so the
check is not simply blocking everything.

### P1-2 — Parity gate: `|worktrees|` vs `|active agents|`

**Mechanism.** A tick check computing registered worktrees, on-disk slot directories, and
live agents, failing on any mismatch. Distinguish the three populations — the inherited
census's 313 registered vs 182 unregistered is exactly a registry-vs-disk divergence, and a
check reading only the registry would have reported health.

**Why it forces.** Rule 6 is currently unmeasured, so drift is invisible until a census.
Note this gate is **incompatible with `AGENTS.md:82` and `:106` as written** — A2 and A3
must be resolved first, or the gate will fail continuously against sanctioned parked slots.

### P1-3 — Point the unpushed detector at slot child repos

**Mechanism.** Extend `ci-hub/health/unpushed_parent_commits.py` (or add a sibling) to walk
`worktrees/*/{hermit,reverie,liteinst2}`. Its own docstring says cost does not scale with
worktree count (`:41-43`), so the objection is already answered.

**Why it forces.** It closes the blind spot in §C5 — the only rule-7 detector that exists
cannot see the repositories where the work actually is.

### P1-4 — Close the loop on fail-closed refusals

**Mechanism.** Route each `holder_liveness.py` refusal into a tracked rescue obligation
(a `tg` task or a ci-hub obligation) rather than a log line, with the documented recovery
SHA that Hard Invariant 14 requires.

**Why it forces.** It converts "correctly refused, forever" into work that is owned and
ages visibly. Do **not** relax the refusal — the fail-closed posture at
`holder_liveness.py:18-22` is what has prevented data loss.

### P2-1 — Correct the contradicted text

Fix A1–A5 in `AGENTS.md`. Specifically: retract the presence-is-harmless framing at `:53-54`
and state rule 3; reconcile `:106` and `:82`/`:131-133` with parity, which most likely means
**deleting "parked" as a sanctioned state**; correct `:285` to make parent `ai_docs/`
transient-only; enumerate the two write locations; and state the rule-5 lifecycle.

**Why it is P2 and not P0.** Text is the weakest control in this report, and the same
corpus already demonstrates that a documented rule survives a night of being ignored. Fix
it because contradictory policy makes the mechanisms above arguable — not because
correcting it changes behaviour on its own.

### P2-2 — Make parent writes structurally coordinator-only

**Mechanism.** `scripts/parent-main-write` is already the single writer for parent `main`
and already refuses a bare commit and a bare push. Extend it to refuse a caller that is not
the coordinator or `hermit-coord`, with a narrow allowance for the rule-2 locations
(`experiments/<assigned>/`, transient `ai_docs/`).

**Why it forces.** Rule 3 becomes a property of the write path rather than of who read
`AGENTS.md:60`. The chokepoint exists; only the identity check is missing.

**Caveat.** I used this tool twice today for authorised parent tooling commits, so the
allowance must be real or legitimate work stops. This needs the owner's call on who counts
as coordinator, which is why it is P2 rather than P0.

---

## Honest limits of this audit

- The census numbers are **inherited from the dispatch and not independently verified**.
  No finding above depends on them.
- I did not audit dispatch prompt texts; they are not in the repository.
- I verified that no *call site in this repository* passes `cwd` to a spawn. Spawning may
  occur through ORC-internal paths outside this tree; the observable — agents landing in the
  shared parent — is consistent with `cwd` being unset, but I did not trace the spawn
  implementation itself. **P0-3 is what would settle this by measurement** rather than by
  my inference, which is a further reason to build it early.
- I did not attempt to fix anything. This is analysis only, per the task.

---

## Appendix — findings index

| # | Bucket | Rule | Evidence |
|---|---|---|---|
| A1 | Contradicted | 3 | `AGENTS.md:51,53-54,60-63` |
| A2 | Contradicted | 6 | `AGENTS.md:106` |
| A3 | Contradicted | 5, 6 | `AGENTS.md:82,131-133` |
| A4 | Contradicted | 2 | `AGENTS.md:285` |
| A5 | Contradicted | 1 | `AGENTS.md:138-139` |
| B1 | Not documented | 2 | `AGENTS.md:153` (sole clause); `.llms/skills/hermit-coord/SKILL.md` silent |
| B2 | Not documented | 2 | no file enumerates the two write locations; cf. `AGENTS.md:60,283-289` |
| B3 | Not documented | 3 | absent corpus-wide |
| B4 | Not documented | 5 | `AGENTS.md:129-139` describes parking, not retirement |
| B5 | Not documented | 2 | transient vs durable undefined |
| C1 | Unenforced | 2 | `~/.orc/orc.d.ts:477,483,447`; zero call sites |
| C2 | Unenforced | 1 | `AGENTS.md:29-34`; `.orc/plugins/hermit-dev/index.ts:739-762` |
| C3 | Unenforced | 7 | `scripts/release-worktree.rs:8-9,47-48,65,68-69` |
| C4 | Unenforced | 6 | `ci-hub/health/tick-hub.yaml:90-101` |
| C5 | Unenforced | 7 | `tick-hub.yaml:195,198`; `unpushed_parent_commits.py:74,80,41-43` |
| C6 | Unenforced | 6, 7 | `ci-hub/health/holder_liveness.py:4-8,11,18-22`; `residue_sweep.py:225-238` |
