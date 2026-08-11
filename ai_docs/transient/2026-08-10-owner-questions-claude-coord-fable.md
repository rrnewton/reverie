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

- **Active worktree slots: 17, against a documented cap of 12** (16 when first counted; my team
  added one). Plus 4 `released` and 2 `owner-lease-revoked` rows, 23 total.
- **Top-level `claude`/`codex` agent processes: 22, against a cap of 15.** (Counted as
  processes whose parent is not itself one, per the invariant's own definition. The invariant
  already records 16 on 2026-08-08 and notes it was over then.)

I have been routing new work into slots I already own rather than allocating more, which
respects the spirit but leaves the count wrong. The honest options are (a) the caps are
current and the fleet must shed slots/agents, or (b) the caps are stale for a 316-core box
and should be raised to a number that matches how this machine is actually used. Either is
fine; silently operating over a documented invariant is not.

**Question:** raise the caps, or enforce them?

### Q8 addendum — the registry is CORRECT; the alarm is a vocabulary mismatch; and a fourth slot state exists that no cap governs

Reconciled on 2026-08-11. **Neither side is wrong** — all three views agree exactly:

| view | count |
|---|---|
| physical directories under `worktrees/` | 23 |
| `ACTIVE.md` managed rows | 23 |
| registered git worktrees (union of `hermit`/`reverie`/`liteinst2`) | 23 |

**Zero disk-only paths, zero row-only entries, zero unregistered directories.** The failure mode
`AGENTS.md` warns about — a worktree on disk with no row, or a row pointing at a deleted worktree —
is simply not present. Nothing needs preserving or attributing.

**The allocator's "23 physical slot dirs against an advisory 12" warning is apples-to-oranges:** it
counts *physical directories* against the *active* soft limit. The honest breach is smaller and
differently shaped:

- **17 active against a cap of 12 — over by 5.** A genuine Invariant 13 breach.
- **6 retained non-active against a parked allowance of 5 — over by 1.**

**The bookkeeping defect worth naming:** those 6 are not *parked* in the policy sense. Parked means
"clean, detached, cache-retained, and **omitted from `ACTIVE.md`**". These are clean, but they still
hold branches (not detached) and they are still listed. So `released` and `owner-lease-revoked` rows
occupy neither the active cap nor the parked allowance, and **no counter in the policy actually
governs them.** That is a policy gap, not an agent error — worth closing whichever way you rule on
the caps themselves.

**No work is at risk in the six.** All are clean (zero dirty files in every child), and every branch
is either merged to `main` or pushed to `origin`. Three (`cadence2`, `scorecard`, `slot04`) hold
branches with **literally zero unique commits** — they point at commits already on `main`
(`b5a4748e` is #2023's merge, `0c6f20ea` is #2018's). I have authorized reclaiming only those three.

**Two cannot self-heal without you.** `drainer-5` and `slot03` are `owner-lease-revoked`: leases
never expire, their owners are gone, so per the adoption rule nobody can bind the lease and
`--clean` will keep refusing indefinitely. They will sit there until someone overrides. Note
`drainer-5` carries `codex/versioned-strict-verify-receipt`, the branch behind **open PR #1749**,
which is a live port job — reclaiming its disk is safe, but the PR is still wanted.

## Q9 — `agent-utils` pin is in drift; I deliberately did NOT repair it mid-drain

`make check-agent-utils-pin` reports `state=drift`:

```
ERROR: canonical checkout branch is "", expected main
ERROR: checkout is not exactly origin/main (ahead=0, behind=11)
ERROR: parent gitlink 59da8697... does not match checkout d2fdfc83...
ERROR: parent gitlink is not exactly origin/main (ahead=0, behind=1)
```

The checkout is clean (`checkout_dirty=false`), just detached and behind. This predates my
session — the parent's `git status` showed `M agent-utils` at handover.

**Why I left it alone.** `AGENTS.md` says the peg must equal fetched `rrnewton/agent-utils:main`,
so the documented repair is to advance the checkout and commit the exact gitlink. But an
`agent-utils` gitlink bump is precisely what caused the CPU-boxing incident earlier this session:
it activated `DEFAULT_SMALL_CPU_COUNT = 1` and silently 1-core-boxed seven build steps, costing
~2.5 minutes on every validate until it was diagnosed. Advancing it now — with four agents
mid-drain, the box-exclusive validate lock in constant use, and validation already unreliable
(#2111) — risks perturbing the one measurement everything else depends on, and any resulting
red would be attributed to whichever PR happened to be validating.

It is a hygiene violation, not an active breakage: validates are running and the drift is one
commit on the gitlink, eleven on the checkout.

**Question:** repair it now and accept the risk, or hold until the pool is drained and validation
is reliable? My recommendation is to hold, then bump deliberately with a validate immediately
before and after so any behaviour change is attributable. I have not acted either way.

## Q10 — Four detcore product decisions, each blocking PRs, none resolvable by a merge

These came out of characterizing the 41-PR `detcore*` cluster. Each is a **model choice**, not a
conflict to resolve, and between them they block roughly twelve PRs. All four were verified in
code, not inferred from titles.

**1. chown: emulate or forward? (#1851 vs #1918 — direct contradiction)**
#1851 moves `chown/fchown/fchownat/lchown` from PassThrough to
`SyscallClassification::Determinized` and answers them with `handle_ownership_change_noop`
returning `Ok(0)`, documented as *"the success is not observable through a later stat."*
#1918 adds `translate_virtual_root_ownership`, rewriting `arg1`/`arg2` so the **real host chown
runs**. Under #1851 the family never reaches `ExecuteOriginal`, so #1918 becomes dead code for the
family it was written for. **Landing order silently decides which one is a no-op.**

**2. flock: which copy? (#1742 vs #1759 — identity, not similarity)**
Both PRs' `syscall_classification.rs` post-image is the **byte-identical blob `32b226dd`** (main is
`a8df8603`). #1742 additionally carries `detcore/src/fd.rs +78/-0` — `flock_mode`, so a contended
blocking *conversion* restores the lock Linux non-atomically dropped. #1759 changes `fd.rs` not at
all, and its declared scope is *"Fence RDRAND determinization off LiteInst, SaBRe and e9patch"* —
the flock work is out-of-scope carry-over of a strictly thinner copy. Recommendation: keep #1742's
flock, strip it from #1759. Cheap, but it is someone's work either way.

**3. DETLOG memory-record format (#1695 vs #1875 — mutually exclusive)**
#1875 deletes `procmaps::display()` and drops the bracketed pathname *by design*; #1695 adds
`display_as(map, "[heap]")`, which depends on that pathname existing. **#1875 deletes the function
#1695 extends.** A combined design is plausible but would be a third PR.

**4. Reverie pin: three mutually exclusive candidates**
#1975 (`108f9ab4`, pre-rename), #2097 (`4664b5e5`, the only one rebased past DBI→DBT), #1767
(`7c67585f`). Only one can be current.

**Question:** rulings on 1–3, and confirmation that #2097's `4664b5e5` is the intended pin. I have
not guessed at any of them; they are product-model decisions, not coordination.

## Q11 — detcore's bottleneck is REVIEW, not validation, and I cannot fix it alone

Batching does not help here, and my own reasoning about why was backwards — an agent corrected it:
folding N critical PRs yields **2 reviews at one head instead of 2N reviews at N heads**, so folding
*reduces* review cost. The problem is the absolute number.

Of the 36 non-getrusage detcore PRs, **31 carry no review label at all and 9 are
`post-facto-human-review`.** These are Hermit syscall handling, which is *critical* under your
2026-08-10 policy, so two different agents should review each — roughly **70 outstanding reviews**
against a cluster where only 5 PRs have any review.

Two things make this tractable rather than hopeless, and one makes it your call:

- **Cross-model review works and is worth its cost.** A Codex review of #2113 returned
  REQUEST-CHANGES with four real defects that the authoring agent's own content-identity proof
  could not surface — because that proof binds *content presence*, a different property from *guard
  efficacy*. This is concrete evidence for Q1.
- The detcore cluster is also **116–165 commits behind main**, so most of it needs a rebase before
  review is even meaningful.

**Question:** should this team spend its capacity on detcore review throughput — running paired
Claude+Codex reviews against rebased heads — or stay on the drain lanes and leave detcore for a
team that owns that code? Reviewing ~70 critical determinism changes is a large, slow commitment
and I would rather be told to make it than assume it.

## Q12 — Reverie pin currency is strict equality, so every Reverie commit invalidates every outstanding Hermit receipt (probable root cause of the backlog)

This is the most consequential thing found tonight and I think it explains the drain far better
than anything else in this document.

`ci-hub/qualifying_receipt.py:572`, inside a function whose docstring reads *"Assert strict
mutable-tip currency exactly once before merge"*:

```python
if row["reverie_base_sha"] != current_reverie_base:
    return BaseVerdict.REVERIE_BASE_NOT_CURRENT
```

**Strict equality, not ancestry.** A Hermit receipt is admissible at land time only if the Reverie
SHA it was validated against *is* Reverie's current tip.

Measured 2026-08-11: Hermit `main` pins `99437f05e82377…` across five `Cargo.toml` files; Reverie
`origin/main` is `349460925ee56f…`. **Reverie moved twice in a few hours** (`99437f05 → 4664b5e5 →
3494609`).

**The consequence.** Every Reverie commit invalidates every outstanding Hermit receipt at once.
Combined with a **box-exclusive validate lock at ~7 runs/hour shared fleet-wide**, the window in
which a Hermit PR can be validated *and* landed before Reverie moves again is narrow, and it
resets for the whole queue each time. This was found by bisecting why `land-pr.sh` refuses
#2113 at step 5b with the exact-head authority passing twice; it is not specific to that PR and
will block every Hermit landing through that lander.

It also cascades: landing a pin bump immediately invalidates every *other* pending receipt,
including the ones that were green a minute earlier.

**Question:** is strict tip-equality the intended rule while Reverie is under active development?
Three plausible relaxations, none of which I will pick unilaterally because each trades away real
assurance:

1. **Ancestry instead of equality** — require the validated Reverie SHA to be an ancestor of
   Reverie `main`, so a receipt stays admissible if Reverie has only moved *forward*. Loses the
   guarantee that Hermit was tested against exactly what it will build with.
2. **Equality against the pin, not the tip** — assert the receipt matches what Hermit's
   `Cargo.toml` actually pins, and enforce pin currency separately and deliberately. Decouples
   "did you test what you ship" from "is your pin fresh".
3. **Keep it, and batch pin bumps** — accept the coupling, but land Reverie changes in scheduled
   batches so the invalidation happens on a known cadence instead of continuously.

I have authorized only the conservative path meanwhile: a standalone pin-bump PR to `3494609`,
validated on its own, kept out of any consolidation. An agent correctly refused to bump the shared
cross-repo pin from inside a CI-harness batch, which would have buried it where no reviewer would
look for it.

## Q13 — the worktree registry records a branch at allocation and never writes it back

Small, but it fail-closes unrelated destructive work, and the shape recurs.

`release-worktree.rs --clean` runs a **registry-wide** verifier and refuses to remove *any* slot
while *any* row is stale. Tonight an authorized reclaim of three already-merged slots was blocked
by two unrelated rows:

```
DRIFT slot=claude-1     hermit recorded=detached                  actual=fix/e2e-applications-hermetic-guest-cwd
DRIFT slot=drain-corpus hermit recorded=drain/test-corpus-batch-1 actual=drain/test-corpus-batch-4
worktree-registry: FAIL rows=23 correct_rows=21 drift_rows=2 product_cells=69 drift_cells=2
```

Both drifts are **benign in origin**: the registry records the branch at *allocation* time and has
no write-back when an agent legitimately switches branches, so any long-lived slot accrues drift
simply by being worked in. Both were our slots and both are being re-recorded through
`allocate-worktree.rs`, which is the sanctioned adoption move. Nobody forced or hand-edited
anything.

The fail-closed design is right — refusing a destructive operation while its model of the world is
known-stale is the correct trade. Two cheap fixes would preserve that while removing the false
block: a branch write-back on the allocate/release path, or treating recorded-vs-actual branch
drift in an *active* slot as advisory while keeping it fatal for the slot actually being cleaned.

**The remedy already exists and nothing invokes it.** `allocate-worktree.rs --repair --slot SLOT`
is documented as *"Reconcile recorded branch cells from physical checkouts"*, supports `--dry-run`,
and is safe by construction — it is scopeable to one slot and **refuses to rewrite a cell for an
unpublished checkout**, so it cannot record a branch nobody can fetch. It works. It is simply never
called on a branch switch.

Two further data points sharpen this:

- **The obvious repair is a silent no-op.** I told two agents to re-run their allocation line — the
  documented *adoption* move for an unbound lease. One verified it does nothing to the branch cell:
  `allocate-worktree.rs` writes `hermit_branch` only from an explicit `--hermit-branch`, and
  defaults it *only when the field is absent or `-`*, so an existing stale value (including the
  literal `"detached"`) survives while the `updated` timestamp refreshes. It looks like it worked.
  My error, but the failure mode is the tool's.
- **Drift regenerates faster than it is repaired.** Within the hour, one row was fixed and a *new*
  one appeared — `fix-drcache` moved to `bump/reverie-pin-3494609` doing authorized work. The count
  stayed at two with a different composition. This is not a backlog to clear once; it is a
  continuous function of agents working normally.

Worth noting this is the same defect shape as the classification census triple (#2118) and the
`mechanism:`-label discipline (#2116): **a recorded value that must be manually kept in sync with
reality, with nothing enforcing the sync.** Three instances in one night suggests it is a pattern
worth naming in policy rather than fixing three times.

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
