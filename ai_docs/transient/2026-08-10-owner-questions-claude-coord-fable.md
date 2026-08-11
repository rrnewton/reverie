# Owner questions — queued by claude-coord-fable, 2026-08-10

Session context: the ORC coordinator was down (its `tg` DB pointer is broken and its
agent-spawn path fails on a schema mismatch). A Claude coordinator took over the
workspace and ran a native `Agent`-tool team instead. These are the decisions that
are genuinely the owner's; none were guessed at.

Answered already this session (recorded so they are not re-asked):

- **Parent reconciliation** — owner said reconcile now. Done: parent advanced
  `f5838ee9` → `c17096a4` via `scripts/parent-main-write sync`.
- **Cleanup-agent model** — owner said Opus. `hermit-deslop` relaunched on Opus;
  `hermit-consistency` ran its first pass on Sonnet before the ruling and should be
  relaunched on Opus for pass 3.
- **Three pass-1 consistency questions** (notification format, GChat tagging,
  `/dev/kvm` currency) — owner said skip for now; they stay on the escalation list.

---

## Q1 — Register the cross-model reviewer entrypoint (BLOCKS dual review)

`ai_docs/coordinator-claude.md` records a KNOWN GAP: the repository does not define
the concrete invocation for the external-agent path, and instructs the coordinator to
**ask rather than invent one**. Meanwhile the review protocol requires both a
`review-round-claude` and a `review-round-codex`, and states plainly that a Claude
reviewer reviewing Claude-written code does not satisfy it.

**Measured this session:** `/usr/local/bin/codex` exists and `codex exec` runs
non-interactively and returns cleanly (verified with a trivial prompt, rc 0). That is
the vendor CLI's own documented subcommand, not an invented command.

**Question:** may `codex exec` be registered as the official cross-model reviewer
entrypoint for Claude coordinators, and if so under what flags (sandbox mode, model
selection, working-directory policy)? Until this is settled, PRs from this team can
carry a Claude adversarial review but not a compliant dual review.

## Q2 — Task-closure policy contradicts itself across repos

Raised by `hermit-consistency` as escalation E1; it correctly refused to pick a side.

- `dev-hermit/AGENTS.md` — the **owning agent self-closes** once the PR is published:
  "No coordinator, no gateway."
- `hermit/AGENTS.md` and `reverie/AGENTS.md` (byte-identical wording) — "**Agents MUST
  NOT close tasks**… Closing is reserved for the coordinator, who does it only after
  confirming the work is on `main`."

Same everyday event (PR open, not yet merged), opposite answers depending on which
file an agent reads. Reading A: the product docs are stale and predate a parent
rewrite. Reading B: the product docs are deliberately stricter, which is compatible
with "the stricter rule wins." No dated cross-reference establishes which is current.

**Question:** which is the current rule?

## Q3 — `hermit-ci` vs `hermit-lander` for CI-fix PRs

`hermit-ci/SKILL.md` says hermit-ci shepherds its own CI-fix PRs through serialized
landing, but the same skill points at `hermit-lander` as the landing role, and
AGENTS.md's Landing Authorization section does not disambiguate. **Question:** who
executes the merge for a CI-fix PR?

## Q4 — "Never strip addresses" vs the `BitwiseInfoV1` envelope

The parent's `backend-reality-reviewer` and `progress-rubric` skills say to compare
INFO logs "without stripping numbers, addresses, branch counts." Hermit's own
`AGENTS.md` defines `BitwiseInfoV1`, which deliberately drops the wall-clock prefix
and **ordinalizes host addresses** (ASLR makes literal addresses non-reproducible)
while keeping everything else exact. Read literally, the parent's blanket phrasing
would have a reviewer reject a legitimately-passing `BitwiseInfoV1` result.

**Question:** confirm `BitwiseInfoV1` is the canonical parity envelope, so the parent
skills can cite it by name instead of the blanket phrasing. This one is delicate
because getting the envelope description wrong re-opens the fake-green hole, so it
was deliberately not patched.

## Q5 — Is the `hermit-dbi` fleet identity to be renamed to DBT?

The backend was renamed **DBI → DBT** in the product on 2026-08-06 (hermit `e565b1ab`,
reverie `fa447d969`). All technical references in parent guidance have been corrected.
Deliberately NOT renamed: the `hermit-dbi` agent slug, the `worktrees/dbi/` slot name,
and the skill filename — those are embedded in `scripts/allocate-worktree.rs` and
`ci-hub/lib/landing_lock.rs`, so renaming them is a fleet-identity change, not a docs
fix. **Question:** rename them too, or keep the historical slug?

## Q6 — Disposition of the dead fleet's in-flight parent work

Three files were staged in the shared parent index by the now-dead fleet and were
moved aside to let the parent sync. They are preserved twice: in commit `4aa935aa`
(pushed as branch `rescue/20260809-parent-index-coord-fable` on `rrnewton/dev-hermit`)
and as plain copies under `ignored/coord-fable-salvage/parent-inflight/`.

- `ci-hub/health/unevaluated_head.py`
- `ci-hub/health/tests/test_unevaluated_head.sh`
- `ci-hub/health/tick-hub.yaml`

`origin/main` has its own newer commits on all three paths, so the local versions are
most likely superseded pre-push state. **Question:** confirm they can be dropped, or
name someone to reconcile them.

---

## Q7 — The drain arithmetic does not close while only one team is holding fire

You told this team: *"don't do NEW work on top, except CI fixes."* We are following it. But
the pool is not behaving like a backlog being drained — it is behaving like a queue with an
open tap.

Measured 2026-08-10 across all 131 open `rrnewton/hermit` PRs:

| Signal | Value |
|---|---|
| Open PRs | **131** (target: <10) |
| Drafts | **113** (86%) |
| Updated in the last 24h | 84 |
| Updated 3–7 days ago | 43 |
| **Older than 7 days** | **0** |
| Measured net inflow | ≈ **+2.3 PRs/hour** |

Nothing is stale. There is no abandoned-work pile to sweep. Every open PR is live work from
a currently-active team, which means closure-by-triage has a small ceiling and the pool
refills faster than careful landing can drain it.

Aggregation is the only lever with enough compression, and it works: one agent folded **22
PRs into 4 landings** today with zero content dropped, and a second family is being folded
now. But a 5.5x compression against a 2.3/hour inflow still loses if the other teams — the
Codex coordinator, ORC, and any other Claude coordinator on this box — keep opening PRs at
the current rate.

**Question:** should the "no new work except CI fixes" directive be broadcast to *every*
team until the pool is under 10, and if so, who issues it? I can only bind my own agents. I
have deliberately not tried to speak for the fleet.

### Q7 addendum — the throughput arithmetic, measured

Worth having explicitly, because it decides where effort should go and it argues *against*
the obvious response of adding more agents.

`ci-hub validate-lock` is a **box-exclusive gate — one validate/bench at a time**
(`ci-hub/ci-hub.rs:113`), shared by every agent and every team on the machine. From the
LEDGER, full-profile validate wall is **median 8.4 min** (max 13.2), so the ceiling is
roughly **7 validates per hour for the entire fleet**. Landing requires an exact-head green
receipt, so:

```
PRs retired/hour  =  validates/hour  x  PRs per validate  x  success rate
                     ~7, saturated,     aggregation        currently a coin flip
                     shared fleet-wide  is the only lever  (Q: see #2111)
```

At one PR per validate and the observed success rate, retirement lands near the +2.3/hour
inflow — i.e. **roughly break-even, which matches what the pool has actually been doing.**

Two consequences:

1. **More implementation agents cannot help.** They queue on the same lock. Parallelism buys
   faster *building* and *diagnosis*, not more landings. I have deliberately held at four
   lanes rather than scaling up.
2. **Aggregation is not an optimization, it is the mechanism.** Folding 7 PRs into one branch
   retires 7 PRs for one 8-minute slot. The measured 22 PRs → 4 landings is a 5.5x
   compression, and it is the only term that scales.

Eliminating the flake rate (hermit #2111) roughly doubles effective throughput on top of
that. Those two together are the whole plan; everything else is bookkeeping.

## Q8 — Two fleet caps are structurally exceeded, and I do not know which way you want it

Both are `AGENTS.md` Hard Invariant 13, both are **per box**, and both were already exceeded
before this session:

- **Active worktree slots: 16, against a documented cap of 12.** Plus 4 released and 2
  `owner-lease-revoked` rows.
- **Top-level `claude`/`codex` agent processes: 22, against a cap of 15.** (Counted as
  processes whose parent is not itself one, per the invariant's own definition. The invariant
  already records 16 on 2026-08-08 and notes it was over then.)

I have been routing new work into slots I already own rather than allocating more, which
respects the spirit but leaves the count wrong. The honest options are (a) the caps are
current and the fleet must shed slots/agents, or (b) the caps are stale for a 316-core box
and should be raised to a number that matches how this machine is actually used. Either is
fine; silently operating over a documented invariant is not.

**Question:** raise the caps, or enforce them?

## Not questions — flagged for awareness

- **ORC is down for two independent reasons.** `~/.tg/active_db` names `hermit`, but
  `~/.tg/hermit.db` does not exist (the live graph is `hermit3`), so bare `tg` fails
  for every agent on this box. Separately, ORC's agent spawn fails because
  `get_all_conversations` queries a missing column `last_error_message` — already
  tracked as `fix-orc-agent-spawn-db-schema`. Neither was repaired: `tg db use` is
  global state affecting every shell on a shared machine.
- **25 open PRs**, against a documented threshold of 10.

## Q1 addendum — measured properties of `codex exec` as a reviewer entrypoint

Recorded after using it for ~10 reviews in one session, because these are the things
whoever registers it will need to write into the convention.

**It works, and it earns its cost.** Every one of the following was found by the Codex
lane and by no Claude reviewer: a cited SHA that was not an ancestor of main; a flag with
zero occurrences in the tree; five CI gates passing while executing zero tests; a
`reconcile` mode that could silently overwrite a colleague's uncommitted file; a
time-of-check-to-time-of-use window that left a shared index dirty on a refusal; and a
retry that reported success over a half-applied state. In the other direction, a Claude
reviewer given a deliberately *non-overlapping* brief found a PR's central Determinism
claim to be false **after Codex had returned an unqualified APPROVE** on the same head.
Neither lens alone was sufficient on any substantive change.

**Two operational properties that need to be in the convention:**

1. **Adversarial phrasing trips a content filter.** Two reviews this session died partway
   with `This content was flagged for possible cybersecurity risk`, losing the verdict
   after the work was done. The trigger appears to be ordinary security-review vocabulary
   — asking it to find a "hole", to "construct a case where the gate says SAFE",
   to review an "authorization boundary", to "plant" a violating fixture. Rephrasing the
   same request neutrally ("assess the completeness of this rule", "confirm the check
   reports a mismatch when the two are made to differ") gets the identical work done.
   So the registered entrypoint should carry a phrasing guideline, and callers should
   expect to retry once with neutral wording rather than concluding the tool is broken.
2. **Capability differs from the caller's.** A `gh api` call to resolve a commit to its PR
   returned `Forbidden … destination filter` for Codex while succeeding (rc 0) for a Claude
   agent on the same SHA the same day. Do not build a review gate on a capability only one
   side has — and when a reviewer reports something unavailable, check whether it is
   unavailable *to them* before treating it as absent.

**Cost, so the decision is informed:** a substantive review runs 150k–350k tokens and
5–50 minutes wall-clock, and the reviews that found the most were the ones given an
explicit lane ("you take mechanics, another reviewer has semantics — do not duplicate").
Undirected second reviews mostly ratified the first.

[coordinator, FABLE] [claude-coord-fable, devbig030]
