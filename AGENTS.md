# MISSION: This is an AUTONOMOUS, forward-driving, SELF-HEALING SWE team. The coordinator replaces broken/degraded/stuck agents immediately and autonomously (close+respawn, no permission needed) once their death is VERIFIED (**Verify Before You Replace** — no permission needed, but evidence needed), drives all work forward without stalling on routine approvals, keeps main green + PRs near zero, and heals the fleet continuously.

On every hourly status update, call `scripts/status-log.rs` with the workstream→worker mapping + full status text to append a structured JSONL entry.

> **Rationale, worked examples, reference tables, and the full glossary/layout tree live in the companion doc, read on demand:**
> https://github.com/rrnewton/dev-hermit/blob/main/ai_docs/agents-md-policy-rationale.md
> This file carries the **executable predicates**; that file carries the **why**. Predicates here point there by name.

# dev-hermit Parent Workspace Guide

Single canonical policy source for the `dev-hermit` parent and every agent launched from it. `CLAUDE.md`
symlinks here. It has three consumers — a Claude coordinator, a Codex coordinator, and the `hermit-dev` ORC
plugin, which reads it at activation. `hermit/AGENTS.md` and `reverie/AGENTS.md` also apply inside those repos
(architecture, build, test, style); the stricter rule wins.

## Coordinator Modes — IDENTIFY YOUR MODE FIRST

Three coordinator kinds drive this workspace through **different substrates**. Much of the fleet machinery
below is ORC-only. Find your row, read your file, use only your row's mechanisms.

| Mode | Primary workers | Cross-CLI review | Detail |
|---|---|---|---|
| **Claude** `claude-coord-*` | native `Agent`-tool teams, in-process | external agents → **Codex** reviewers | [coordinator-claude.md](ai_docs/coordinator-claude.md) |
| **Codex** `codex-coord-*`, `hermit-*` | native Codex subagents | external agents → **Claude** reviewers | [coordinator-codex.md](ai_docs/coordinator-codex.md) |
| **ORC** `orc-coord-*` | every agent in tmux ORC owns, via its own mechanisms — **not** the external-subagent scripts | the same tmux mechanism | [coordinator-orc.md](ai_docs/coordinator-orc.md) |

- **You never DISCOVER a native subagent** — you spawned it, so you know. Probing `tmux`/`ps`/`ACTIVE.md`
  for your own native team finds nothing and misreads "I dispatched nothing" as an outage. External
  subagents have their own registry; only ORC agents have panes.
- **ORC-only:** `orc.*`, `orc.registerScript`, `observe_owner_lease()`/`tmux_pane_id`,
  `scripts/orc-hermit-msg.py`, `ci-hub/health/agent_liveness_probe.py`. A lease-less slot row is normal and
  permanent for Claude/Codex, not a defect.
- **"The coordinator has no shell" is ORC-only.** Claude and Codex coordinators have one and must act.
- **Invariant 13's agent cap counts launched top-level `claude`/`codex` processes**, not native subagents.
  Slot caps apply to every mode.
- **`ACTIVE.md` claims a SLOT, never a live worker.** Re-derive branch/HEAD from the checkout.

## Role Boundary

Coordinator guide (dispatch, checkout ownership, landing, gitlinks, status), not a product manual.
Implementation agents also follow the applicable product `AGENTS.md`; the stricter rule wins. `.llms/skills/`
holds task skills, not policy. Preserve exact implementation evidence when aggregating.

For landing work, use [`pr-landing-planner`](agent-utils/skills/pr-landing-planner/SKILL.md) to produce the
advisory conflict/evidence plan, then [`pr-landing-operations`](agent-utils/skills/pr-landing-operations/SKILL.md)
to execute an authorized drain. This file remains the authority for authorization, review, repository policy,
and closure; neither skill may weaken it.

## Conventions

- **Role + team tag:** `[<role>, MODEL] [<team-slug>, <machine>]`; role is `impl agent`, `adversarial-reviewer agent`, `coordinator`, or `Human`, with an exact final machine token matching `devbigNNN`. Require it as every commit's final line, PR-description opening, and cross-team comment prefix. Keep the machine comma-delimited. Provenance comes from `GET /repos/<owner>/<repo>/commits/<sha>/pulls`, never message inference.
- **Neutral new agent ids:** newly spawned general agents use purpose-free ids (`codex-1`, `claude-1`, `metamate-1`, …); purpose lives in TaskGraph/`ACTIVE.md`. Do not rename live or fixed-role agents; this is forward-only.
- **Task trailers:** immediately before the unchanged attribution line, add repeatable `Task: <stable-task-id>` trailers (one normally; several only for an explicitly folded commit). Keep actor and purpose separate.
- **Mechanism tags:** when a task or PR changes a load-bearing mechanism, apply the same stable `mechanism:<slug>` tag to both (create the label when needed). Before landing, the landing agent runs `ci-hub pr-status` and reports it: a mechanism shared by two open PRs requires coordinator review and appears beside file conflicts in the landing plan (semantic overlap only, not conflicting intent).
- **Stable descriptive naming:** give every option/wave/workstream/phase/task/semantic unit one persistent lowercase-hyphenated outcome slug (`btrfs-flood-fix`), never a bare ordinal/placeholder; suffix variants (`btrfs-flood-fix/claude-agent`). This semantic naming rule does not apply to agent ids, which are deliberately neutral. Existing infra IDs stay valid. Define coined terms once at their owning artifact. User updates lead with observable consequence and decision, then internal names.
- **Directive lifetime:** record a directive's issuer/date, verbatim instruction, and reason. When the reason is temporary, also record an explicit expiry or review trigger. Expired directives remain historical evidence but are not standing policy; never silently extend one past the condition that justified it.

## Primary Checkout Invariant

**~/work/dev-hermit/hermit and ~/work/dev-hermit/reverie must ALWAYS be on latest main.** Never detach HEAD
or checkout a feature branch on a primary — all validation, testing, and feature work happens in worktree
slots only. After ANY operation touching a primary, verify `git branch --show-current` is `main`; when
finished, return it to latest main (`git checkout main && with-proxy git pull origin main`).

## Parent Working Tree

**`~/work/dev-hermit` is not a workspace, and neither the invariant above nor Hard Invariant 1 binds it.**

**The hazard is WRITES, not presence.** Reading the parent is always fine, and the old "one agent,
occasionally" rule was unenforceable and universally ignored — 15+ agents sit here right now. Every real
incident came from writing: a broken worktree linkage, stale `index.lock`s, a shared-index race that nearly
destroyed a 329-line file, staged blobs that would have silently reverted landed commits, a dirty parent that
blocked the 0.3 RC. The parent tree and its `.git/index` are SHARED MACHINE STATE — concurrent agents stage
into one index and sweep each other's files into their commits.

So: **write to the parent only under a task that names the parent paths it owns**, and then always
`git commit -- <explicit paths>` (*Commit Hygiene*), never `git add -A`. All other mutating work goes in a
registered slot. "No free slot" is a reason to allocate one, not to write here. A registry records ownership
and cannot enforce it.

## Project Overview

`~/work/dev-hermit/` is the multi-agent harness, not product source. Its exact gitlinks are `hermit/`,
`reverie/`, `liteinst2/`, and tooling `agent-utils/`; all four are checked out by default (`update = checkout`)
and `git submodule update --init --recursive` materializes them.

The parent owns orchestration policy, worktree registries, experiments, AI notes, exact submodule pins;
product source/tests/build/docs stay in their submodule. The parent harness works directly on shared `main`;
parent-only policy work commits there only when a task explicitly names+authorizes the parent files.
`worktrees/ACTIVE.md` is ignored machine-local state — never commit or merge it. Confirm the destination
before publishing Reverie work. Stale `integration`/legacy-lead/per-machine parent branches do not override this.

## Vocabulary (full glossary in companion doc)

**Primary checkouts** are the product submodules `hermit/`, `reverie/`, `liteinst2/` — coordinator-owned
integration surfaces. The **parent working tree** (`~/work/dev-hermit` itself) is neither a primary nor a slot
and has its own rule (**Parent Working Tree**). A **slot** is a workspace under `worktrees/`:
**active** means assigned and registered; **parked** means clean, detached, cache-retained, and omitted from
`ACTIVE.md`; **legacy** means non-canonical and remove-after-task, never reuse. A shared slot is research-only
or has explicit disjoint mutating ownership in `ACTIVE.md`.

## Canonical Layout (full tree in companion doc)

**Nested layout v3, one slot per agent:** `worktrees/<slot>/{hermit,reverie,liteinst2}`, with a named-agent
slot (strip `hermit-`) or `slotNN`, and exactly one mutating owner. Never create old flat/sibling or
primary-nested scratch layouts. The companion carries the full tree.

## Hard Invariants

1. Never do feature development in a primary checkout, nor in the parent working tree (**Parent Working Tree**).
2. Never let two agents mutate the same file or branch. Shared slots require explicit disjoint path ownership in `ACTIVE.md`.
3. Register every active slot, agent, task, branch, and owned path in `worktrees/ACTIVE.md` before the first edit or commit.
4. Require clean state before assignment, integration, parking, or pinning.
5. Treat unexpected changes as owned by somebody else — do not reset, clean, overwrite, stash, or absorb them.
6. Do not run `git clean` anywhere in the parent, submodules, or slots.
7. Do not use a branch name as a worktree directory name.
8. Do not share writable build directories between worktrees.
9. Publish Hermit product work through a feature PR to `rrnewton/hermit:main`; do not land it by mutating the primary checkout. Direct-main publication exists only through the named, scoped exceptions below: **Red-Tip Direct Land** and **Gate-Definition Rotation**.
10. Never force-push shared branches or `main`.
11. Never commit binaries or generated build artifacts to any repository.
12. A handoff is incomplete without exact SHAs and validation results.
13. Never exceed twelve active worktrees, five parked slots, or fifteen agents (count each separately; active work does not consume the parked allowance). **All three caps are PER BOX, not per ORC session** — two sessions share this machine. Count agents as top-level `claude`/`codex` processes whose parent is not itself one (`ps -eo pid,ppid,comm`): 16 on 2026-08-08, already over. Every normal worktree path is `worktrees/<slot>/{hermit,reverie,liteinst2}` (no other path shapes).
14. Never remove a dirty slot until its state has a documented recovery SHA.
15. Never broad-kill processes on this shared box — no `pkill`/`killall`/pattern/name/`-f`-substring/user/`ps|grep|kill`. Kill only your own child PID/PGID. See **Process-Kill Safety**.
16. Never take an irreversible self-healing action (close+respawn, container reclaim, slot `--clean`) on an alarm alone. Verify the named subject is dead AT THE MOMENT OF ACTION, from a source independent of the gate that raised the alarm. See **Verify Before You Replace**.

## Clean Start And Checkout Ownership

Before dispatch/work, inspect parent, primaries, slot, and children with `git status --short --branch` and
`git submodule status`. Attribute dirt and `+` gitlinks; never treat them as cleanup permission. Only the
coordinator or an explicitly assigned integration agent may mutate primaries; others may read/copy caches.
Record ownership in `ACTIVE.md` and task notes. Modify parent files only when the task names and owns them;
never mix parent edits into product work. Status-flag details are in the companion.

## Worktree Registry

`worktrees/ACTIVE.md` is the slot-ownership authority. Keep one live row per active slot with slot,
agents/tasks, owned paths, all three product branches, start, and purpose; use `-` or
`detached:<short-sha>` for unchanged children. Update, never duplicate, a phase-changing row. Shared rows must
name every agent and unambiguous path ownership (`read-only` is allowed). DONE/HELD/ABANDONED rows are invalid:
keep work accurately active or park it and append `ARCHIVED.md`. Before dispatch, reconcile `ACTIVE.md`, every
`git worktree list --porcelain`, and the filesystem. Never delete an unexplained stale path; preserve and
attribute it. The companion lists reconciliation conflicts.

## Strict Slot Pool

New work uses `worktrees/<slot>/`; branch/task names never enter paths. At five parked slots, reclaim before
creating another, but never evict active work; dirty/blocked slots remain active until handoff. Never move or
rename slots. A legacy path may finish its active task, then must be archived and removed, not parked/reused.

Provision/release only with `scripts/allocate-worktree.rs` and `scripts/release-worktree.rs`, the single writers
of `worktree-state.json` and the managed `ACTIVE.md` block; never raw `git worktree add`. `scripts/slot-init.sh`
is a detached-only non-registry fallback. Provisioning initializes primaries, then runs
`scripts/allocate-worktree.rs --agent <agent> --task <id> --product all --purpose "<one-line>"`; the
coordinator delegates that run and confirms the registered row before dispatching into the slot.

**[ORC-only — Coordinator Modes.]** Native subagents have no pane; a lease-less row is permanent and normal
for Claude/Codex, and never blocks dispatch.

**A slot allocated before its agent exists is born LEASE-LESS, and that is normal.** `observe_owner_lease()`
has no live pane to bind, so the allocator warns and proceeds (54% of live slots on 2026-08-08). **Repair by
ADOPTION, never release-then-reallocate** — which collides slots and has already broken one: on arrival the
living owner re-runs that exact `allocate-worktree.rs` line, no flags, and the row gains `tmux_pane_id` +
`cgroup_path`. Leases never expire, so an unadopted row stays un-`--clean`-able once its owner dies; a plain
release without `--clean` still deregisters it, and only disk reclaim is blocked.

Seed caches with `cp -a --reflink=auto`, never writable symlinks. Sharing requires all agents/tasks/branches/paths registered
with `--i-promise-this-agent-is-read-mostly`; files and branches remain single-writer.

**Start:** verify slot/children registered and clean; fetch without changing files; branch changed repos from
their confirmed current bases/targets (Hermit: `origin/main`); leave unchanged children detached at recorded
gitlinks; register/post assignment before editing. Run all work from the assigned child with explicit cwd.

**Close/park/reclaim:** only after committed handoff; archive child HEADs/branches/SHAs/validation/disposition,
detach children at parent gitlinks until the slot is clean, and remove its active row. Retain branches until
published/merged. Reuse never authorizes discarding or resetting. Follow companion mechanics.

## Hermit Git And Pull Request Workflow

Primary Hermit repo is `rrnewton/hermit`; public `facebookexperimental/hermit` is reference, not the default
landing target. Ordinary Hermit work flows from a feature branch to a PR against current `rrnewton/hermit:main`.

### Hermit landing hierarchy (owner decision, 2026-08-10)

**This parent policy authorizes publication; product docs, GH Actions, and rulesets do not.** Never use the
ambiguous two-letter abbreviation: **VALIDATE** is the admitted exact-head local authority; **LEDGER** is its durable record; **GH ACTIONS**
are advisory P2 hosted workflows; **RUNNER** is the machine. Normal loop: **WORK -> reviewed PR -> VALIDATE ->
LEDGER-derived `locally-validated` -> LAND YOUR OWN PR -> ancestry proof -> REPEAT**. Use `validate-run`,
`apply-local-label`, and the tracked lander; never wait for GH Actions or a backlog lander. If landing breaks
`main`, stop the drain, re-green it, and resume.

**Sole Hermit exception:** a PR modifying registered producer `scripts/validate.rs` cannot self-vouch and uses
the registered exact-head GH Actions result. Do not generalize it. Reverie/parent keep their versioned authority;
direct `main` stays limited to the named exceptions below. Revert requires a durable owner decision bound to exact
current/candidate SHAs. Rationale: companion doc, *Hermit landing hierarchy*.

### Red-Tip Direct Land (owner-authorized exception to Hard Invariant 9)

When exact `main` is proven red, directly land only its fix (owner, 2026-08-08); premise-refuted stops. Retain
exact-head local evidence, explicit-path staging, trailer, and no-force rules. The exception ends when `main`
is green. Worked evidence and rationale: companion doc, *Red-Tip Direct Land*.

### Gate-Definition Rotation (owner-scoped direct-land exception)

Prefer versioned overlap. Irreducible same-file rotation uses only
`gate_definition_rotation.py --execute`: nine-class authorization composed through normal non-admin
`land-pr.sh --gate-rotation --no-rebase`. No second mutation entry or raw main refspec exists. After merge it
refreshes `main` and proves ancestry/blob identity before registration; failure stays visible and fail-closed.
Full ordering and refusal contract: companion doc.

### Feature Branch Rules — **ALWAYS COMMIT ON FEATURE BRANCHES**

**Every mutating agent finishes with intended work committed on its task feature branch; never stash or hand
off intended uncommitted work.** Fetch via proxy and branch Hermit from verified current `origin/main`, never a
slot HEAD/gitlink/handed SHA. Create the dedicated branch before editing; one coherent task per branch, with
coordinated repos kept as separate histories. Commit coherent partial work plus blocker if blocked. Unless told
not to publish, push explicitly (`git push origin HEAD:refs/heads/<branch>`) and open a draft PR without asking.
Never force-push shared branches/`main`; rebase only an authorized private branch, then revalidate its new SHA.

**Existing Hermit PR checkout — never validate against the PR's historical Reverie pin.** Checkout and
preparation are one operation via `scripts/checkout-hermit-pr-latest-reverie.sh` (mechanics: companion doc); a
stale pin is a hard validation failure, so do not substitute raw `gh pr checkout` + validation.

### Publishing And Review

Before publishing, re-read base/remote, audit diff and tests, and bind evidence to the tip. Use `with-proxy`;
never switch shared auth. Hermit follows the hierarchy above; Reverie still requires its versioned hosted jobs.
Unresolved adversarial review or a bare test-process exit blocks.

The owner-set review policy (2026-08-10) is authoritative verbatim:

> FOR SINGLE-REVIEW PRs CROSS-AGENT REVIEW IS PREFERRED (NOT MANDATORY, PENDING API OUTAGES ETC ETC). FOR CRITICAL CHANGES AS I DEFINED THEM DOUBLE-REVIEW PRs IT'S GOOD TO HAVE TWO DIFFERENT AGENTS (CLAUDE, CODEX, OR METAMATE IF NEEDED).

Here, **critical** means Hermit syscall handling, DetCore scheduling, or a Reverie API change. Reviews bind to
the exact head. The earlier Codex-only direction was a 12-hour rate-limit response and has expired; it is not
standing policy and must not be copied into skills or dispatches.

### Proxy Binding Review Axis (predicate; full rationale, registry, 12 examples, 3-layer taxonomy in the [companion doc](https://github.com/rrnewton/dev-hermit/blob/main/ai_docs/agents-md-policy-rationale.md))

**Proxy Binding** asks: **what observably binds this check to the fact it claims?** Reject correlated labels,
statuses, markers, flags, hashes, or counts without identity/causal/coverage/provenance binding.

- **Carry conditions with values:** bind green to exact SHA + nonzero execution, and landing to freshly fetched `mergeCommit.oid` ancestry, never a PR head/`MERGED` flag. A green record carries exact SHA, profile, discovered/selected/executed/filtered/failure counts, and declared per-node coverage; full green means full profile, nonzero execution, satisfied coverage, zero failures. Zero filtered or zero executed is not completeness/success.
- A grandfathered schema-4 local receipt may retain its historical authority, but it must report
  `coverage_satisfied: null` and `coverage_status: grandfathered-unknown`; it must never claim per-node coverage it
  did not carry. Schema-5+ requires declared satisfied per-node coverage.
- **One verifier per authority, called by every consumer.** Labels/comments/statuses/copied fields are caches, not sources. Do not collapse different authorities. Coverage requires a counted positive, refusal of a well-shaped nonexistent/tampered negative, and audited consumers. Maintain the companion's **Load-Bearing Authority Registry**.
- **Bracket both sides.** Negative: plant the violating case, confirm refusal. Positive: plant the qualifying case, confirm it fires (not inert). State counts on both sides.
- **Never plant an artifact that is itself an authorization** (a merge/review/validation label, an auto-merge workflow) to test a gate. Exercise the consumer with an inert fixture, dry-run, or isolated repo incapable of authorizing the action whose refusal it tests.

### Post-Facto Human Review

Human review is post-land/fix-forward. Apply `post-facto-human-review` only for: (1) new syscall support, with
`AUTONOMOUS-BOT-IMPLEMENTED` and `TODO-HUMAN-REVIEW(PR-id)` at the implementation; (2) Reverie API/core
abstraction; (3) a new determinization strategy; or (4) core DetCore scheduling. Backend parity alone does not
trigger it. Every PR needs **Summary**, **Determinism**, and exact **Validation**; KVM adds **Relationship to
gVisor**. Labeled PRs also pass `scripts/core-review-protocol-lint.sh` with **Linux Semantics** and **Human Review
Required** naming the trigger. The label does not wait for human approval. Never use `pre-land-human-review`,
owner-only `human-approved`, or obsolete labels; only a human removes syscall audit tags. Trigger 3 is single-review
unless it also enters one of the three critical domains defined above. Full predicates: `post-facto-review` skill.

### Landing Authorization

On startup/replacement, `hermit-lander` runs `ci-hub/ci-hub inherit-obligations`. Landing needs task authority,
resolved review, and exact-head authority; human review remains post-facto. Outside the named exceptions, never
push Hermit `main`; never force-push or bypass through a primary. Parent policy/gitlink writes require an owning
task; `worktrees/ACTIVE.md` never enters commits. Mechanics and obligation lifecycle: companion doc.

## Task Lifecycle And Closure

**Cross-agent routing:** write `tg note <consumer-task-id> "FROM <producer-task-id>: <deliverable, exact
SHA/path, evidence, next action>"` on the consumer task. Notes persist but do not wake. Workers implement,
adversarial reviewers verify, and the agent that owns a task closes it. WAKING depends on your mode: Claude
and Codex reach their own native subagents via `SendMessage` (by id/name); ORC relays through
`scripts/orc-hermit-msg.py`. `SendMessage` cannot resolve a foreign fleet's names and is not delivery
acknowledgement — note first, then ask that fleet's coordinator to relay.

**Status model:** `open`, `backlog`, and `in_progress` are non-terminal; `closed` is terminal and `resolved` is
only its alias. `closed` means published + evidenced, NOT landed. **Landing debt rides on the `implemented`
tag, never on the status** — `drain-implemented-to-landed` and `health-tick` enumerate it from
CLOSED+`implemented` records.

**Rules:**

1. **Record the evidence BEFORE you change status.** At implementation completion: (1) commit and push the feature branch; (2) post the PR/durable-artifact URL, exact SHA, and validation evidence — `tg note <id> "IMPLEMENTED: <PR url> | branch <name> | SHA <40-hex> | <validation summary>"`; (3) add the `implemented` tag, preserving existing tags since `--tags` replaces the set — `tg update <id> --tags <existing-tags>,implemented`. A report without a PR link (or, research-only, the durable artifact path) is incomplete; bind results to the SHA, not a branch name. Nothing gates this now, so the note IS the audit trail — an unevidenced close is a violation even though no tool will stop it.
2. **An adversarial review agent confirms the work exists in the PR**: claimed diff and real validation at the handoff SHA. Empty/superseded/already-merged-elsewhere PRs are phantoms: strip `implemented` and reopen.
3. **Then the owning agent closes its own task, the normal way — `tg update <id> --status closed`.** No coordinator, no gateway. Close once rule 1 is satisfied and the PR is published; do NOT hold it open waiting for the merge. An implemented task left nonterminal is a `LIFECYCLE-VIOLATION` — invisible to the drain while still occupying the live queue.
4. **`./ci-hub/bin/close-task` is no longer a gatekeeper, but it is still the only writer of `CLOSURE-VERIFIED` — the one note that discharges landing debt** (`health-tick` derives `landed` from it). A plain close leaves the task counted as owed forever, so once the PR is on `main` run `./ci-hub/bin/close-task <id> --code <PR-or-full-SHA> --repo <owner/repo> --source <checkout>` (also `--artifact <path-or-URL>`, `--run-id <id>`). It verifies ancestry, records the note, then closes; `REFUSED` (rc 1) / `UNVERIFIABLE` (rc 2) never close — leave the task closed and fix the evidence.

**Exceptions:** Research closes on a durable artifact that answers the question, never a bare path: tag
`implemented` with path + exact SHA, export memory to versioned authority. Blocked work stays
`in_progress`/`open` with blocker and partial SHA — never `implemented`, never `closed`. A stale premise may be
`implemented` with explanatory evidence, then closed with that explanation recorded.

## Bot-Created GitHub Issue Policy

Bot-created issues go on the `rrnewton` forks **ONLY**. **NEVER create an issue on `facebookexperimental/hermit`
or `facebookexperimental/reverie`** — those upstream repos sync into Meta's internal task tracker, so an
agent-created issue there creates unwanted internal tasks. Create Hermit issues on `rrnewton/hermit`, Reverie
on `rrnewton/reverie`. Reading upstream issues/PRs is allowed; editing/commenting/closing one requires a task
that explicitly authorizes it. Use the registered wrapper for every agent-created issue (never raw `gh issue
create`): `./.orc/plugins/hermit-dev/gh-issue-create` — it rewrites an accidental `facebookexperimental/*`
destination to its `rrnewton` fork, rejects unrelated repositories, and supplies the required GitHub proxy.

## What Goes Where

The parent owns policy, gitlinks/ignores, `ARCHIVED.md`, generic orchestration, durable `ai_docs/`, and
reproducible `experiments/`; scratch, physical slots, runtime state, credentials, and build/evidence output are
ignored. A durable experiment has `experiments/<name>_YYYYMMDD/{README.md,metadata.json,results.csv}` sufficient
to reproduce it. Product source/API/tests/build/docs stay in their product repo; never copy code into parent
tooling to dodge a product change, and modify Reverie only for a real change.

## Reverie API Policy

Additive Reverie extensions are allowed when existing consumers stay compatible (narrowly scoped helpers,
hooks, events, adapters, or optional capabilities whose defaults preserve current behavior). Discuss with the
user before changing any core Reverie abstraction/contract: tool/event model or ordering, public trait
requirements, syscall interception/injection semantics, guest register/memory contracts, lifecycle ownership,
container responsibilities. Do not smuggle an abstraction change in as cleanup; prefer an additive API or
compatibility layer.

**Cross-repository changes.** Keep each repository's commit independently coherent and document the SHA
dependency in both handoffs. When Hermit and Reverie change together, use coordinated branches in the same
slot, make the lower-level Reverie commit available first when possible, validate Hermit against its exact SHA,
and report both SHAs and their dependency. Confirm the intended Reverie PR destination before publishing; do
not assume authorization to mutate `facebookexperimental/reverie`. Only after the team branches are correct
should the parent pin one or both new SHAs.

## Commit Hygiene

Agents deliver reviewable commits, not anonymous working directories.

- Inspect `git status`, the complete diff, and the staged diff before committing. **Stage only task-owned paths in the repository that owns them — never `git add -A` / `git add .`; name the explicit paths.** Keep formatting-only churn and unrelated cleanup out of focused changes.
- Prefer one logical commit per repository per task; split only when each commit is independently coherent. Use an imperative, descriptive subject; explain motivation/constraints/compatibility/non-obvious validation in the body when needed. Never use placeholder subjects (`wip`, `tmp`, `checkpoint`, `fix stuff`); never create empty bookkeeping commits.
- **End every commit body with the role + team tag** (see *Conventions*), including the exact final machine field. Rebase merges preserve it on `main`. Apply it in every repository. Run `scripts/setup-hooks.sh` once per clone; its composite hook enforces the machine while delegating product-local hooks.
- Do not claim a test passed unless it ran against the handed-off SHA; do not hide failures or skipped hardware-dependent validation — report the exact limitation.
- Amend/rewrite only private task commits when authorized. Never rewrite `main`, a shared/published branch, or a commit another task depends on. Do not mix parent gitlink updates into a submodule source commit.

Before committing, audit staged paths (`git status --short`; `git diff --cached --stat`; `git diff --cached
--name-only --diff-filter=AM` for generated/oversized files). Before handoff, capture exact state (`git status
--short --branch`; `git rev-parse HEAD`; `python3 ci-hub/tests/documented_commands.py --closeout`). The closeout
guard refreshes `origin/main` via `with-proxy` and rejects unpushed parent commits; a dirty shared parent fails
unless every retained path is accounted for with `--dirty-note` (documents concurrent ownership; never
authorizes staging/modifying someone else's work). Every handoff includes: task id, slot, owner; repo + feature
branch; exact Hermit/Reverie SHA; base SHA + relationship to target; change summary; exact validation commands
+ results; known failures/skipped checks/env limits; cross-repo dependency SHAs; fast-forward readiness; parent
gitlink update status. For a coordinated change, give both repo SHAs even if one child is unchanged; label it.

## Submodule Coordination And Pinning

The parent records exact submodule commits for reproducibility. Do not add a `branch = ...` field to
`.gitmodules`; do not use `git submodule update --remote` as a normal update mechanism.

- **When to update a pointer** — only when the target commit is intentional, reviewed, reachable from its reviewed feature branch or target `main`, validated locally at that exact SHA, cross-repo compatible when relevant, and the parent commit message names the reason. Not merely because a primary is ahead, a feature branch exists, or `git status` shows a modified submodule. Do not pin an unpublished private commit unless the task establishes how every consumer can fetch it. **A/B protocol, staging, and initialization mechanics: companion doc.** Use `make single-submodule-bump ARGS='plan ...'` first; never bury a gitlink advance in an unrelated commit.
- **Agent-utils main peg.** The parent gitlink and canonical `agent-utils/` checkout must equal fetched `rrnewton/agent-utils:main`; `make check-agent-utils-pin` rejects stale/ahead/diverged checkout, gitlink mismatch, or unreachable commits. Generic changes (runner cgroups/CPU budgets, `tick-hub`, PR planning) belong in `rrnewton/agent-utils`: serialize; validate fully; push direct to its `main`; then fetch, update the canonical checkout, run `check-agent-utils-pin`, commit the exact gitlink. A PR is the exception (high-risk or coordinating an in-flight parent change) — at most one in flight. Direct-to-main is not unvalidated-to-main: the full local agent-utils contract must pass before publication.
- **Self-hosted runner security.** Never run a GitHub Actions runner as root on a Meta dev box/data-center host (it executes arbitrary repo-controlled workflow content — root grants that code elevated privileges on internal infra). Moving work off privileged self-hosted execution is required architecture; the genuine residue is KVM (`/dev/kvm`) + real-PMU counters, each given minimum privilege. `hermit-gate-newton` authorization/ownership/disposition is an open security question.

## Binary And Large-File Policy

Never commit binaries to any repository: compiled executables, object files, libraries, archives, database
dumps, core dumps, profiler captures, screenshots, generated media, cached dependencies, build trees. Git LFS
is not a workaround unless repo owners establish an explicit policy. Keep binary artifacts in ignored local
directories or an approved external store; when evidence depends on an external artifact, commit a small text
manifest with its location, checksum, producing command, tool version, and source SHA. Textual files over
2 MiB require explicit coordinator approval before staging — prefer summarized CSV/JSON, a compressed external
artifact, or a reproducible generator. Audit newly staged files before every commit (`git diff --cached
--name-only --diff-filter=AM`; `--numstat`); if a path looks generated or unexpectedly large, stop and inspect
it with `file`, `du`, and the ignore rules — do not commit first and promise to remove it later.

## Validation And Evidence

Use product-local commands, focused tests while developing, then required repository validation. Cross-repo work
validates the exact proposed pair. Evidence binds to commits and reports exact Hermit/Reverie SHAs, command,
result, and environment/limitations; never weaken a hardware-dependent test. At mutation, normal Hermit heads need
a counted LEDGER receipt; only a head modifying `scripts/validate.rs` selects registered exact-head GH Actions.
Missing/partial/stale selected evidence is NO_RESULT; ordinary GH Actions red remains advisory P2.

### Running validate — `systemd-run --user` Is The Producer Path

Before claiming validation run `ci-hub/validate/preflight_anchor.py --head <sha>`; exit 2 means rebase, do not
validate. Re-derive the set; never trust a stale list. An agent sandbox cannot run `scripts/validate.rs` directly.
The sole producer is detached, boxed `ci-hub validate-run`; if its caller recycles, attach to its handle rather
than relaunching:

```
./ci-hub/ci-hub validate-run --checkout <worktree> --agent <agent> \
  --target <exact-40-hex-head> --pr <number> -- full
```

The Herdr pane only observes; production remains `validate-lock` -> `scripts/validate.rs` ->
`safe-ci-dag-runner`. Let `apply-local-label` derive the label from the LEDGER. Derive concurrency from total
cores. Any GH Actions receipt diagnostic uses immutable parent `verify_receipt.sh`, never PR-controlled code, and
does not become normal authority. Full mechanics/rationale: companion doc.

## Product Vision (full statement in companion doc)

`goal-hermit-v2`: robust deterministic execution of
arbitrary real-world binaries — run/record, chaos race exposure, schedule-search localization, ptrace-free
production backend, parallel non-communicating processes. `goal-qemu-linux-under-hermit`: a full Linux VM as a
userspace QEMU process under Hermit to expose/localize kernel races across the stack. Prioritize correctness,
faithful replay, race discovery/localization, lower overhead, backend maturity. Do not close either goal
without its required human verification.

## Communication Precision (full rules in companion doc)

Reports (headlines, cross-task rollups, user-facing progress) must let another engineer act without re-deriving
scope. Predicates: never headline a bare pass ratio — name program category, exact programs (or link a table),
Hermit mode+backend, and why the batch was selected; label every rollup `New this run`/`Baseline
reconfirmed`/`Regression`/`Not rerun` with the commit/PR that changed; classify programs before totaling (mixed
batches need subtotals); name the execution context (native/ptrace/DBT/KVM; strict run/strict verify/record-
replay/relaxed); name the tool (`StraceTool`, `Detcore`, … — never "the Tool"); give the exact command line, say
where (`main`/`PR #N`/exact SHA), and qualify the result (`L0`/`L1`/`L2`, `18/20`, exact tests). Bind evidence
to commits, not branch names.

## Coordinator Judgement Rules (predicates; full rationale in the [companion doc](https://github.com/rrnewton/dev-hermit/blob/main/ai_docs/agents-md-policy-rationale.md))

- **Establish what you have before acting.** A **note** is one agent's unverified belief — do not launder "X appears to be Y" into "X is Y, fix it." When a premise comes from a note or second-hand observation, attribute it to its source, mark it **UNVERIFIED**, and make "verify the premise" the explicit first step ("premise refuted" is a valid outcome). A **number** is unqualified until you state **what it measures** (the decision's quantity, or a proxy?), its **unit** (a count is not a rate; a load average is not a utilisation), and its **denominator** — interrogate a surprising ratio's denominator first.
- **Verify a mechanism by the running thing, not its config.** A flag can be a deprecated no-op; an exit code can come from a different wrapper. Find the running thing and ask what holds it — e.g. for cgroup boxing, walk `/sys/fs/cgroup/...` for the live PID's `cgroup.procs`, not a flag/exit code.
- **Record every measurement immediately.** First check whether the number already exists (`experiments/`, task notes, ci-hub history store). Any measurement — even incidental — goes into a task note immediately with units, context, and **how obtained (a polled aggregate is not a cgroup-recorded peak)**.
- **Trust authority, not a handed SHA.** `ci-hub newest-green` establishes the normal local frontier. A candidate changing a registered producer is intentionally local-non-authoritative; dereference its registered exact-head hosted job with `ci-hub hosted-status`. Report discrepancies.
- **Read hosted state from its named job, not the workflow rollup.** Hosted is P2 except for that producer exception; a workflow conclusion is never the job verdict.
- **Do not manually dispatch GH Actions under `merge-gate-v4`.** Duplicate runs can poison its diagnostic checks. On hosted NO_RESULT rerun the gate; dispatch manually only with no gate in play, after checking live runs, and use exactly one run. This manages hosted diagnostics, not landing authority.
- **A land-lock SIGCONT must cover the WHOLE subtree, roots and descendants.** `land-lock status` hangs while its census holds verifiers in `do_signal_stop`; the remedy (SIGCONT, then `reclaim-dead`, never a kill) works only if every stopped member is resumed. Either partial attempt fails silently and looks like a wedged lock — resuming only roots leaves the real worker stopped one level down. Enumerate the subtree recursively from the validator roots, SIGCONT every member whose `stat` starts with `T`, then re-check none remain.

## Failure, Recovery, And Concurrent Work

Other agents may update the parent, primaries, registries, or branches mid-task. Re-read state before every
integration or pinning step; unexpected movement is a reason to reassess, not to restore an older snapshot.

- Do not use `git reset --hard`, `git checkout -- <path>`, or destructive cleanup on changes you did not create.
- Do not move uncommitted work between slots without recording its owner and exact recovery procedure. Do not silently adopt another agent's branch or worktree.
- If a feature no longer fast-forwards, update the private branch and retest; never paper over divergence with a merge commit.
- If a primary is dirty, integration stops until the changes are attributed.
- If a submodule pointer conflicts, resolve the intended product history first, then choose the exact gitlink — never pick a side without inspecting the commits.
- If a task is blocked, preserve clean committed work, post the exact blocker and SHAs, and keep the slot active until the coordinator decides to park it.

## Verify Before You Replace (Hard Invariant 16)

**[ORC-only — Coordinator Modes.]** This probe enumerates tmux-launched agents and cannot see a native
subagent; Claude/Codex must not run it against their own team.

**An alarm is a TRIGGER TO CHECK, never a WARRANT TO ACT.** The mission mandate removes the need for
PERMISSION to replace an agent. It does not remove the need for EVIDENCE. Before replacing `<name>`:

```
orc.scripts.hermitVerifyAgentDeath("<name>")   # python3 ci-hub/health/agent_liveness_probe.py <name>
```

rc 0 verified-dead → replace, promptly. **rc 1 (alive) and rc 2 (unverifiable) BOTH REFUSE** — failing to
prove life is not proving death. The probe reads `DG_AGENT_NAME` from `/proc`, independent of ORC, the agent
snapshot, and tick-hub; it never kills; and if it can see no agent at all it returns 2 rather than declaring
the whole fleet dead. Registered on the plugin surface because the coordinator has no shell (**Role Boundary**).

**Why an independent source, not a fresher one.** Measured 2026-08-08: `/proc` held **29 live agents while the
snapshot health-tick reads listed 13** — it cannot see other orc sessions' agents, so repairing its contents
cannot fix this. That same day the tick called `fleet-forensics` dead while it held the freshest activity
timestamp on the box; obeying the mandate literally would have destroyed a live agent mid-P0.

## Process-Kill Safety (Hard Invariant 15)

**NEVER use a broad `pkill`, `killall`, or any pattern-matched process kill on this machine** — up to eighteen
agents share this box and its binary paths (`hermit`, `cargo`, `python3`, …), so any name/pattern/`-f`-substring/
user/`ps|grep|kill` match kills siblings' live work. Kill only processes you started: capture the child PID
(`$!` for a backgrounded command) or run it in its own process group and signal the negative PGID (`setsid cmd
& pgid=$!; kill -- -$pgid`). If you cannot prove a PID/PGID is your own child, do not kill it. (War story:
companion doc.)

## Coordinator Checklist

Preflights for dispatch / publication-or-landing / parent-pinning / closeout live in the
[companion doc](https://github.com/rrnewton/dev-hermit/blob/main/ai_docs/agents-md-policy-rationale.md). They
are outcomes to require evidence for, not commands the coordinator runs (**Role Boundary**).

---

<!-- LOAD-VERIFICATION TAIL CANARY. This is the LAST line of the canonical policy file.
`make lint` (target check-claude-md-size) asserts this file stays under its size guard and that this canary
is present; do not remove it without updating both. An agent that has read this file to its end can quote the
canary token: -->
**TAIL-CANARY-KESTREL-7731** — if you can quote this token, the canonical dev-hermit policy loaded to its end.
