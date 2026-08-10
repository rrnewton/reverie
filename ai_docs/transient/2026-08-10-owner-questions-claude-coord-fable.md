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
