# Owner Directive Register

**Purpose.** The acceptance criteria for the final auditor review. Every entry quotes the
owner verbatim, cites the prompt that created it, and states a **verified** status.

**Built by** `claude-reviewer` (opus-5), task `trawl-all-owner-directives-from-disk-and-hermit1db`,
2026-08-09.

**Sources, both owner-authorized.** Trawled from disk, not from memory or task titles:

| source | orc session | db | owner-authored prompt blocks | span |
|---|---|---|---|---|
| `hermit1` | `4fb50e87-5d91-4294-90fd-…` (name `hermit`) | `hermit1.db` (was `hermit.db`) | **2 608** | 2026-07-20 → 2026-08-08 |
| `hermit2` | `1b176094-9503-4d6a-ac4a-…` | `hermit2.db` | **99** | 2026-08-08 → 2026-08-09 |

Extraction: `content_blocks` where `role='user'` and `user_source` is either a direct
TUI/Web submission or a GChat message with `"is_owner":true`. 2 707 blocks → 1 173
directive-shaped → 1 231 deduped normative sentences → 386 hard-imperative. Raw
extracts under `/tmp/dirtrawl/` (scratch; regenerate with the query above).

---

## 0. How to read the three states

| state | means | bar |
|---|---|---|
| **IMPLEMENTED AND VERIFIED** | I dereferenced the mechanism at a fresh SHA **this session** and it behaves as directed | evidence quoted inline |
| **CLAIMED BUT UNVERIFIED** | prose, a task, a doc, or a landed commit says so — I did **not** confirm the running behaviour | the dangerous state |
| **NEVER IMPLEMENTED** | I looked for the mechanism and it is absent, or it is present and contradicts the directive | evidence quoted inline |

**A closed task is not evidence.** Measured by the owner: 82 % of 235 closed tasks
carry no closure verification.

**The dominant false-negative class, and I fell into it myself.** A directive can be fully
implemented on `main` and still look unimplemented from any worktree that predates it. I
reported "main fails the reverie-pin lint" after running a checkout **67 commits behind**
`main`. It was wrong. Every NEVER IMPLEMENTED verdict below was re-derived against
freshly fetched `main`.

---

## 1. Is the existing tracker trustworthy? (asked first, answered first)

`ci-hub/directives/` — `ledger.json` + `check.py` (728 lines) + `README.md` + `tests/`.
Landed dev-hermit `94c363cf`; the ledger records its own existence.

**Verdict: trustworthy on the axis it measures, and the suspected failure mode is REFUTED.**

The health tick reported `source_rows=16 records=21 satisfied=19 gated=2
task_in_archived_db=21` with `summary: no directive drift`. The concern was that the 19
"satisfied" verdicts rest on tasks in a database the tracker cannot read.

They do not. **The ledger stores no status field at all** — every state is computed at
check time. `satisfied` comes only from `_verify_landing` →
`remediation/protocol.py verify-landing`, which fetches `origin/<target>` and runs
`merge-base --is-ancestor`. Proven by planting defects on ledger **copies** (the real
ledger was never touched):

| planted defect | result |
|---|---|
| hermit row given a real **agent-utils** SHA (wrong repo) | satisfied 19 → **17**, row → `ancestry=fetch_failed` |
| dev-hermit row given a well-formed **nonexistent** SHA | same flip; **fails closed**, never passes |
| satisfied row given a task id in **no** database | satisfied 19 → **18**, `state=task_not_found`, **`ancestry` still `ancestor`** |

The third is decisive: the ancestry leg computed and passed while the task leg
independently demoted the row. The legs are orthogonal; a task pointer cannot manufacture
a `satisfied`.

All 19 carry `ancestry=ancestor` against **four distinct freshly-fetched tips** —
agent-utils `572f4793`, dev-hermit `f5838ee9`, hermit `b5a4748e`, reverie `99437f05`.

**The tracker is not blind to `hermit1`.** Its own refusal text reads *"exists in none of
the **2** known task database(s)"*. `task_databases()` resolves the live DB then
same-alphabetic-stem archives, so `hermit2.db` admits `hermit1.db` but deliberately not
`hermit-policy-audit.db`. All 21 pointers resolve. `task_in_archived_db=21` means
*"pointer resolves into a database we no longer write"* — a migrate-the-pointer flag, not
a blindness flag.

### The real defect, which is not the suspected one

**The tracker does not report green.** Its own verdict is `state=red`, `exit_code=1`. The
misleading artifact is the **summary string** — `"no directive drift; in_progress=0
gated=2"` describes only the drift and gating axes and silently omits both `state=red` and
`task_in_archived_db=21`. Whatever surfaced that line dropped the state field. Same defect
class, but it lives in the rendering, not the verdict. **Fixing `check.py`'s logic would be
fixing the wrong thing.**

`source_rows=16 → records=21` is not a discrepancy: 16 owner rows expand to 21 because
parents carry cross-repo children (4 records legitimately have `source_row: null`; row 10
legitimately maps to 2 records).

### The 2 gated

| id | gate (verbatim) | gated by |
|---|---|---|
| `dbi-to-dbt-rename` (row 9, 2026-07-30) | *"Hermit PR queue has drained: a repo-wide dbi→dbt rename conflicts with every in-flight feature branch, so it is deliberately deferred until that condition holds"* | a measurable condition — **and it is now stale**: the rename has landed (`e565b1ab` "Rename the DBI backend to DBT"; main calls `ci/run-with-reverie-dbt-budget.sh`). Secretly-landed inverse false positive; needs re-derivation. |
| `unsupported-syscalls-fail-closed-default` (row 11, 2026-08-03) | *"owner decision: flipping a default guest-visible behavior … plausibly trips the post-facto-human-review triggers … Deferred pending the owner's call"* | **you, personally.** Nobody else can clear it. |

### Coverage is the actual gap

The tracker is **scope-limited by design** — its README says *"owner tooling,
configuration, and repository imperatives"*. Of the eight seed directives, **only the
reverie-pin one is even adjacent to a ledger row.** Force-push, worktree ownership, agent
confinement, validate ≤10 min, no admin bypass, numbers-with-provenance, ratio-needs-both-
terms: **none has a ledger row.** They live in `CLAUDE.md`/`AGENTS.md` prose with no
verifier and no obligation record. 16 source rows is the tracker measuring a strict subset,
not a bug. **This register is a superset of it.**

---

## 2. The seed directives

### D1 — Single source of truth for validate ledgers · **NEVER IMPLEMENTED**

> *"What is this? We were supposed to aggressively purge all BS / non-source-of-truth
> ledger locations. We have one ledger that is recorded per-machine and implicitly unioned
> by ci-hub. If that is not the case my DIRECTIVE WAS IGNORED. Find the code if it's
> languashing on a PR or redo it and immediately land to main. you may not disobey my
> directives, they must be implemented promptly. ONE SOURCE OF TRUTH for validate ledgers."*
> — hermit2, 2026-08-09 20:16

> *"the SINGLE source of truth ledger for validates (per-machine), unioned by CI-hub (also
> unioned with github results, optionally, depending on CLI flags to CI-hub)."*
> — hermit2, 2026-08-09 21:49

> *"If the per machine ledger is in dev-hermit github on main branch they will never
> conflict and we can union them as our full dataset. I'm leaning towards that."*
> — hermit1, 2026-08-07 01:40

**Verified:** the exact line the owner hit is `hermit/scripts/validate.rs:3462` —
`"{PARENT_ENV} is unset; recording to the DISCOVERED parent ledger …"`. The ledger path is
**discovered by inference** when `DEV_HERMIT_PARENT` is unset, rather than resolving to one
canonical per-machine location. That is the directive's negation, in production code, on
the path the owner ran himself.

**Do not overclaim the second path.** `/durable/validate-run-ledger.jsonl` appears **only**
in `ci-hub/remediation/tests/test_protocol.py:166,187` — a test fixture, not a live writer.
The live store is `ignored/validate-run-ledger.jsonl` (747 rows). The defect is the
*discovery fallback*, not a second production ledger.

**This directive has no tracker row.** It is the case that proved the register was needed.

### D2 — Reverie pin: ancestry + monotonic forward, never equality · **IMPLEMENTED AND VERIFIED**

> *"Yeah I'm ok with the weakened ancestry criteria. But we should have the rule that on any
> conflict resolution for hermit repo the reverie pin must always pick the newer. It can
> only advance monotonically forward and no one can land a PR that regressed the reverie pin
> older."* — hermit2, 2026-08-08 20:20

> *"We switched the policy to 'require ancestor of reverie/main, and montonic movement
> forward' correct? If so please CLEAN UP any docs like this that are out of date and
> inconsistent."* — hermit2, 2026-08-09 20:12

**Verified at hermit main `b5a4748e`:** `scripts/check-reverie-pin.rs` header reads *"Judge
Hermit's recorded Reverie pin by ANCESTRY and MONOTONICITY"*; line 1131 reads
*"OWNER-APPROVED RULE (2026-08-08): ANCESTRY + MONOTONICITY, not equality."* The equality
wording is **gone from main (0 occurrences)**. The code names the precise hole the owner
caught — ancestry alone would accept a conflict resolution that regresses the pin — and
fail-closes when the monotonicity base cannot be resolved.

**Residual, and it is the docs half of the directive:** `hermit/docs/updating-reverie.md:105`
still says *"Reverie pin updated to latest main"*. The cleanup asked for on 2026-08-09 20:12
is **not** done. Sub-state: mechanism verified, docs cleanup NEVER IMPLEMENTED.

**Deployment lag is real and it bit me.** Any branch based before the change still runs the
equality checker and hard-blocks commits. PR #1851's base (67 behind) does exactly that.

### D3 — Never force-push a branch you do not own · **CLAIMED BUT UNVERIFIED**

> *"Agents may race, but they must fix forward and never rewind work"* — hermit1, 2026-08-03 16:50

> *"I want Rewind protection, but I want bypass of required status checks"* — hermit1, 2026-08-03 15:03

Note the owner's own carve-out: `speculative`/`frontier` are *"essentially throw-away"* and
*"will get force pushed all the time"* (hermit1 2026-07-22 17:43, 2026-07-22 19:51).

**Partially verified:** hermit has a ruleset **`main history protection` — `active`**, which
covers `main`. I did **not** verify that non-`main` shared branches are protected, nor that
any mechanism distinguishes "a branch you own". Prose-level in `CLAUDE.md` (Hard Invariant 10).

### D4 — Coordinator owns worktree slots and the parent repo · **CLAIMED BUT UNVERIFIED**

> *"the dev-hermit repo BELONGS to the coordinator, and is only modified by it"*
> — hermit2, 2026-08-09 15:29

> *"We must only have hermit-coord operate on this space"* — hermit1, 2026-07-23 12:30

> *"Have all changes to the parent repo go through hermit-coord"* — hermit1, 2026-07-22 20:58

Documented in `CLAUDE.md` (*Parent Working Tree*). **No enforcement mechanism found.** The
owner observed the violation himself the same day: *"You forgot and assigned two agents to
the same worktree?"* (hermit2 2026-08-09 15:05). A registry records ownership; `CLAUDE.md`
itself concedes it *"cannot enforce it."*

### D5 — Agents confined to their worktree, except `experiments/` and transient `ai_docs/` · **CLAIMED BUT UNVERIFIED**

> *"the coordinator spawns a subagent with CWD in that worktree, with specific instructions
> to NOT edit files outside of it, EXCEPT for the case where there is an experiments/foo/
> parent repo subdir assigned as well (two write locations), or a parent-level new ai_docs/
> file is always allowed output if it is a transient report (and not a DURABLE REFERENCE
> DOCUMENT that belongs into the inner hermit/ repo, very few of those rarely added)."*
> — hermit2, 2026-08-09 15:29

Documented. Enforcement is instruction-only. **Directly falsified this session by the
owner**: agents were spawned with cwd in the shared parent, causing *"three false
divergences, a submodule yanked mid-run, and an uncommitted file nobody could attribute."*

### D6 — Worktrees short-lived and matching active agents · **NEVER IMPLEMENTED**

> *"worktrees are SHORT LIVED … Thus the worktrees/ directory should PRECISELY MATCH the
> active agents, not leave a giant backlog of stale stuff. … Right now I see 84 worktrees"*
> — hermit2, 2026-08-09 15:29

> *"We must have a perfect 1-1 match of agents to slots."* — hermit1, 2026-07-26 16:09

> *"I want to stick to strict slotNN worktrees … we don't want to keep more than 5
> unused/parked slots"* — hermit1, 2026-07-21 21:27/21:54

**Measured now: 96 worktree directories vs 40 live agents.** Up from the 84 the owner
counted hours earlier. The declared cap in `CLAUDE.md` is twelve active / five parked. The
allocator prints the breach as an **advisory** and proceeds ("*SLOT SPRAWL: 80 physical slot
dirs … advisory soft limit 12*"). **The cap is documented, measured, breached 8×, and
non-blocking.**

### D7 — Validate under ~10 minutes · **CLAIMED BUT UNVERIFIED**

> *"We need a green main and a <10min rock-solid local validate before we can really do any
> development."* — hermit2, 2026-08-08 05:01

> *"Ok this is our P0. We must keep validate sane and under ~10 minutes."* — hermit2, 2026-08-09 20:13

> *"does it pass validate local in ~10min or less?"* — hermit2, 2026-08-09 21:49

The ledger (`ignored/validate-run-ledger.jsonl`, 747 rows) carries `real_seconds` per run,
so the metric is **measurable**. Prior published figure: median 611 s over main-line samples,
only 1 of 3 under 600 s. **I did not re-derive it at current main** — that requires a fresh
full-profile run and I hold no validation lease. Status is unverified, and given the 7×
slowdown incident the owner was tracking tonight, it should be treated as at-risk.

Related and unverified: *"validate itself should have a timeout. Both in the inner c group
steps and the outer c group level"* (hermit2 2026-08-09 22:44); *"ruining validate is never
allowed under any conditions. A 7X slowdown should never land"* (22:34).

### D8 — No admin bypass of merge gates · **NEVER IMPLEMENTED — and the ruleset says so in its own name**

> *"Landing greens with --rebase (never --admin, fetch-fresh, ancestry-verify)."*
> — hermit1, 2026-08-03 12:56

> *"we removed an admin bypass tonight for exactly that reason"* — hermit2 quoted context, 2026-08-08

Nuance the owner himself added: *"I wouldn't say admin never"* (hermit1 2026-08-03 14:27) and
*"I want Rewind protection, but I want bypass of required status checks"* (15:03).

**Verified live:** `rrnewton/hermit` rulesets are
`[('main check gating (admin-bypassable)', 'active'), ('main history protection', 'active')]`.
The check-gating ruleset is **named** `admin-bypassable` and is active. Whether that matches
the owner's 15:03 carve-out or contradicts his 12:56 instruction is **a question only he can
settle** — the two prompts point opposite ways. Flagged as a genuine owner-decision item, not
an agent failure.

### D9 — Numbers always with provenance; a ratio is not a measurement unless both terms are shown · **CLAIMED BUT UNVERIFIED**

> *"We should NEVER EVER EVER report a single NUMBER that is fake, that includes test counts,
> performance, numbers and YES, TIME. All must come from TOOLS and have provenance, not be
> pulled out of your ass."* — hermit1, 2026-07-28 09:58

> *"We should never share 3X 4X 186X relative data without also establishing the anchor number
> it is relative too."* — hermit1, 2026-08-02 16:12

> *"You ALWAYS say what the columns/lines MEAN and how to interpret them."* — hermit1, 2026-08-02 16:12

> *"All numbers and quantitative data must always be presented with provenance and specificity
> regarding what they represent."* — hermit2, 2026-08-08 21:18

> *"Never use fake times is a general consequence of never use numbers without provenance."*
> — hermit1, 2026-08-04 08:55

Partial mechanism exists: *"All 4 published figures now carry measured/total/unit; measured=0
with a published value is refused"* (hermit1 2026-08-07 07:36) — scoped to one report, not a
general gate. **No repo-wide linter.** The owner re-issued this on 2026-08-08, which is itself
evidence it is not enforced.

---

## 3. Directives found in the trawl that are NOT in the seed list and NOT in the tracker

Enumerated because the auditor's acceptance criteria must include them. All are
**CLAIMED BUT UNVERIFIED** unless marked; each cites its prompt.

**Honesty and evidence**
- *"NO NO NO you must never ever apply the human-approved label. That is falsification that is never allowable."* — h1 2026-07-22 23:52. **IMPLEMENTED AND VERIFIED** as policy in `CLAUDE.md`; label is owner-only.
- *"PASSING should never be thrown around without detail again."* / *"Never unqualified 'passing.'"* — h1 2026-07-22 19:00, 2026-07-23 13:04
- *"Don't call anything a 'backend' unless it is a real Reverie backend with the same interface as reverie ptrace."* — h1 2026-07-22 23:56
- *"We don't want BS FAKE functionality littering our code."* + *"mark every stubbed out thing with `// TODO-STUB(issue-id)`"* — h1 2026-07-23 00:30/00:32
- *"it's really bad if we're closing tasks for phantom work that never happened."* — h1 2026-07-26 02:23
- *"Every mode must catch up to the one before it"* / *"Always add NEW programs to coverage"* — h1 2026-07-23 13:04
- *"It's like if you took an integer function and broke it to always return 3."* — h1 2026-07-31 03:11 (the fake-parity anti-pattern; **IMPLEMENTED AND VERIFIED** as the `continuous-virtual-time-is-sacred` skill)

**Process / landing**
- *"Fixing CI is *ALWAYS* a P0."* — h1 2026-07-22 14:14; *"Fixing red main is always P0 and lands directly on main."* — h1 2026-08-07 23:47. **IMPLEMENTED AND VERIFIED** as *Red-Tip Direct Land* in `CLAUDE.md`.
- *"I DO NOT want squash merge -- turn us to rebase plus fast-forward merge."* — h1 2026-07-28 10:40
- *"Every subagent should work until a draft PR is up (AT LEAST) … there should never be some branch floating with no PR."* — h1 2026-07-22 19:03
- *"always push right after committing"* — h1 2026-07-22 19:45/20:58
- *"You should never ever test any PR without rebasing first."* — h1 2026-08-04 14:17
- *"Do NOT leave code languishing in PRs."* — h1 2026-07-24 15:08
- *"Do not land with post-facto-human-review unless there are also the review commits (in fact both the adversarial-review AND the passed one…)"* — h1 2026-07-31 04:21. **IMPLEMENTED AND VERIFIED**: `core-review-protocol` blocks on `passed-review-{claude,codex}`; I saw it fail then pass on #1864 today.
- *"Core scheduling changes should ALWAYS be marked in this way"* — h1 2026-07-29 14:11 (trigger 4)
- *"All demo touching commits must mechanically have adversarial review at least once."* — h1 2026-08-01 15:18
- *"documentation only changes … can be merged directly to main without going through a slow PR process"* — h1 2026-07-22 19:46
- *"Every one of our PRs gets one review. Right??"* — h2 2026-08-09 22:44 — **an open question the owner asked and nobody answered.**

**Safety**
- *"We certainly NEVER EVER want to run `git clean -fxd` in this repo"* / *"Don't call `git clean` NEVER do that."* — h1 2026-07-24 13:37, 2026-07-27 19:24. **IMPLEMENTED AND VERIFIED** as Hard Invariant 6.
- *"Please make sure that only PRs created by rrnewton are allowlisted to run on CI. Do not run CI for arbitrary PRs from other users as they could compromise my system."* — h1 2026-07-21 22:42
- *"we should CERTAINLY never accidently commit another git repo -- or binary files, or overly large log files"* + *"We must monitor our .git repo size"* — h1 2026-07-26 12:41
- *"NEVER create an issue on facebookexperimental"* (paraphrase of *"leave them always on the forks"*) — h1 2026-07-21 21:58. **IMPLEMENTED AND VERIFIED**: `gh-issue-create` wrapper rewrites the destination.
- *"NEVER batch validates concurrently — that manufactures false reds"* — h1 2026-08-07 16:55
- *"we always BOX it and KILL IT cleanly if it exceeds its box"* — h1 2026-08-03 15:33; *"validate must use safe-ci-dag-runner which enforces resource caps"* / *"ci-hub must be ADMISSION control"* — h1 2026-08-07 16:55
- *"Again we were supposed to have cgroup enforcement in rust ci dag runner a long time ago. Where is it?"* — h2 2026-08-09 19:57 — **owner re-asking = not done.**

**Engineering standards**
- *"We must be zero warnings always."* — h1 2026-07-22 12:50
- *"We do NOT ever want backend-specific hacks in hermit."* — h1 2026-07-22 21:13
- *"All Reverie backends must see EXACTLY the same syscalls in the guest."* / *"OF COURSE OUR MMAP LAYOUT MUST BE THE SAME ACROSS BACKENDS."* — h1 2026-07-28 03:43, 2026-08-07 15:57
- *"We must NEVER violate DRY this badly."* — h1 2026-08-07 15:06
- *"We should first of all NEVER have bash grow to that scale (graduate to typed python…)"* — h1 2026-08-03 14:51
- *"Virtual time policy is extremely sacred in this system and must be handled very carefully"* — h1 2026-07-31 20:52. **IMPLEMENTED AND VERIFIED** as a skill.
- *"Detcore never blocks on real blocking syscalls."* — h1 2026-07-24 00:47
- *"validate should just exit in error if you test it on a dirty commit unless you pass --run-on-dirty-tree which we do NOT want agents to use."* — h1 2026-08-03 13:57
- *"when a tool is complete it needs to always print out how long it took"* — h1 2026-08-03 14:37. **IMPLEMENTED AND VERIFIED** for ci-hub: I observed `# ci-hub/hosted-status tool COST ACTUAL wall=0.703s cpu=0.356s`.
- *"TIMEOUT=REFUSAL, never cost."* / *"never duration."* — h1 2026-08-07 09:30/09:33
- *"We do not want ANY silent failure."* — h1 2026-08-08 00:18
- *"keep agent-utils pegged at main"* — h1 2026-08-03 15:50. **IMPLEMENTED AND VERIFIED**: `make check-agent-utils-pin`.

**Reverie (new, this session — largely un-tasked)**
- *"the goal is end-to-end testing of reverie as a proper library … going in the front door using the proper reverie traits … Back end specific tests maybe necessary … but they are far lower priority and unit tests are not super high value in this project."* — h2 2026-08-08 22:02
- *"construct artificial guest programs whose behavior is deterministic … does exactly 10,000 system calls … compare … different reverie back ends"* — h2 2026-08-08 20:37
- *"native execution can become our reference golden behavior rather than the p-trace back end"* — h2 2026-08-08 21:16
- *"I do want stats printing for all reverie backends… we might as well have that in a trait."* — h2 2026-08-09 02:27
- *"It would take extraordinary evidence … for me to believe that any of the other back ends are substantively ahead of the p-trace back end."* — h2 2026-08-08 21:55

**Observability / tooling**
- *"our multisect probe needs to only run that test, not the entire CI suite"* — h2 2026-08-08 22:05
- *"We need all of this kind of metadata to be precise at the per test ID. That's the group.test name format."* — h2 2026-08-09 00:27
- *"File the task to rerun parallel scaling experiments … estimate the plateau for each CI step, that should be its max threads on a given machine."* — h2 2026-08-09 02:27
- *"it seems pretty dangerous that we produced any other scripts. In CI hub or otherwise. Which call the TG command line interface. That is really supposed to be private to orc"* — h2 2026-08-09 22:52. **NEVER IMPLEMENTED**: `ci-hub/directives/check.py:245` shells out to `tg sql`, and at least four other ci-hub tools hardcode the tg DB path.
- *"For critical fixes when main is busted you can have one claude sub agent and one codex sub agent attempt the fix in parallel on different work trees."* — h2 2026-08-09 22:41

---

## 4. What the auditor must be held to

1. **The register, not the tracker, is the acceptance criteria.** The tracker covers 16
   owner rows of tooling/config/repo imperatives; this register lists ~60 distinct
   directives, most with no obligation record at all.
2. **Three seed directives are verifiably unmet right now** — D1 (ledger discovery
   fallback in `validate.rs:3462`), D6 (96 worktrees vs 40 agents), D8 (a ruleset literally
   named `admin-bypassable`, active). D2's docs half is unmet.
3. **Most entries are CLAIMED BUT UNVERIFIED, exactly as predicted.** That is not a
   failure of this trawl; it is the finding. Prose in `CLAIMED.md` with no verifier is
   indistinguishable from an unimplemented directive until someone dereferences it.
4. **Two items are owner-decision, not agent-failure**: `unsupported-syscalls-fail-closed-default`,
   and the admin-bypass contradiction between the 12:56 and 15:03 prompts on 2026-08-03.
5. **One question the owner asked is still unanswered**: *"Every one of our PRs gets one
   review. Right??"*

## 5. Recommended next actions (not taken — this task was a trawl)

- Extend `ci-hub/directives/ledger.json` with the D1/D3–D9 rows so they acquire an
  ancestry-verifiable obligation instead of prose.
- Fix the tick's **summary string** to carry `state` and `task_in_archived_db` — not
  `check.py`'s logic, which is correct.
- Migrate the 21 `task_in_archived_db` pointers to live `hermit2.db` tasks, or record
  deliberately that they are historical.
- Re-derive `dbi-to-dbt-rename`: it appears landed and the ledger still calls it gated.
