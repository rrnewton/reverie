# Purpose

Drive Hermit toward robust deterministic execution of real Linux programs: faithful run and
record/replay, race exposure and schedule localization, a production backend without ptrace overhead,
parallel execution for independent processes, and a reproducible QEMU/Linux path. Keep `main` green,
turn reviewed work into landed work, and reduce the open pull-request queue without weakening evidence.
The capability definition is the [Hermit product vision](ai_docs/hermit-v2-roadmap.md#vision-and-success-criteria);
the dated current-state measurements elsewhere in that roadmap are history, not startup policy.

# Scope and coordinator mode

This is the always-loaded policy core for the `dev-hermit` parent repository and every agent launched
from it. `CLAUDE.md` is a symlink to this file. Inside `hermit/` or `reverie/`, the product-local
`AGENTS.md` also applies. If an inner file contradicts this file or a canonical skill named below, stop
and report the exact conflict; do not silently choose or merge the rules.

Use only the guide for the coordinator substrate that launched you:

- Claude coordinator: [ai_docs/coordinator-claude.md](ai_docs/coordinator-claude.md)
- Codex coordinator: [ai_docs/coordinator-codex.md](ai_docs/coordinator-codex.md)
- ORC coordinator: [ai_docs/coordinator-orc.md](ai_docs/coordinator-orc.md)

An explicit owner directive controls while its recorded reason and lifetime remain current. Without a
more specific owner directive, repair an exact-head red Hermit `main` first, then finish work that is
review-complete and closest to landing, then choose work by user impact and how much other work it
unblocks. A status label, queue declaration, or old task note does not outrank current evidence.

# Highest-consequence rules

1. **Every task starts with an explicit write destination.** The dispatch names the repository,
   checkout or clone, directory tree, branch, and whether the agent may write. “No slot” is not
   permission to write somewhere else.
2. **Do not invent project terminology.** Use words established by the owner, the project vision, the
   historical documentation state named below, or an actual source-code identity. Newer agent-written
   prose does not authorize its own vocabulary. If those sources do not cover a fact, describe the fact
   in plain language; do not give it a new name. A new name asserts that a new concept exists and
   requires the owner. Every coordinator dispatch includes this literal rule: “Agents may not coin terms. Use the
   project's existing vocabulary. If a concept has no existing word, describe it in plain language;
   do not name it. A new name requires the owner.” To check earlier project language, inspect Hermit
   commit `5c5c73f662dfaa15c5e256b1c9c7ce4ff861b6e8` (2026-07-22 13:28 PDT), the commit immediately
   before the first documentation change authored by `hermit-agent@meta.com`.
3. **Every adversarial review checks whether the change lowers the bar.** The coordinator applies the
   same check before dispatching and includes this complete block in every adversarial-review prompt,
   not a pointer to it:

   > GOALPOST-MOVING REVIEW RULE
   >
   > Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
   >
   > Treat each of these as an explicit review target:
   > - weakening an assertion so a test passes
   > - widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
   > - renaming or relabelling so a failure reads as a pass
   > - deleting a check rather than satisfying it
   >
   > A passing test is not sufficient when the assertion no longer discriminates the prohibited behavior. Two live incidents from 2026-08-11 show why this must be in the dispatch prompt:
   > - A harness fix PASSED DELETION-MUTATION AND STILL ADMITTED A BACKWARD PIN: the assertion existed but did not discriminate.
   > - The strict-compat corpus kept the legacy comparator for SEVEN CALENDAR DAYS while labelled L2-bitwise.
   >
   > BOTH SURVIVED REVIEW.
4. **One agent owns a directory tree, not a file.** Never grant simultaneous writers different files or
   subdirectories under the same tree. Observers are read-only. Ownership transfers explicitly; it is
   never inferred from inactivity.
5. **The hazard is writes, not presence.** Reading the shared parent checkout is allowed. Writing it
   requires a task that names the owned parent paths, and any commit names only those explicit paths.
   Never use `git add -A` or `git add .` in shared state.
6. **Treat unexpected changes as somebody else’s work.** Do not reset, clean, overwrite, stash, absorb,
   switch over, or “tidy” them. Record the path and observed state, then stop or work around it.
7. **Never remove a dirty slot without a documented recovery SHA.** A detector’s reclaim list is a set
   of candidates to inspect, never permission to release or delete one. A lease or owner-sidecar TTL
   expiring only makes reclaim eligible: a live or indeterminate owner, or a failed process-use,
   cleanliness, or durability proof, still refuses. A successful reclaim records the reclaimer and
   recovery handoff. Follow
   [worktree-provisioning](.claude/skills/worktree-provisioning/SKILL.md).
8. **Do not repair live infrastructure in place.** Changes to coordinator-executed infrastructure such
   as `ci-hub/` are made in a task-specific disposable clone, tested there, and landed as one complete
   commit before activation. Follow the
   [ci-hub out-of-place change protocol](ai_docs/ci-hub-change-clone-protocol.md).
9. **An alarm is a trigger to check, never a warrant to act.** Before replacement or irreversible
   cleanup, verify the named subject at action time using a source independent of the alarm. For the
   [ORC liveness probe](ci-hub/health/agent_liveness_probe.py), rc 0 means verified dead; rc 1 means
   alive and rc 2 means unverifiable. Both rc 1 and rc 2 refuse replacement.
10. **Never broad-kill on a shared box.** Do not use `pkill`, `killall`, a name or substring match, a
   user-wide signal, or a `ps | grep | kill` pipeline. Signal only a PID or process group you created and
   recorded. If ownership is not provable, do not signal it.
11. **A note is one agent’s unverified belief.** Attribute second-hand claims and verify their premise
   before acting. “Premise refuted” is a valid result.
12. **A number is unqualified until it names what it measures, its unit, and its denominator.** A count
    is not a rate; a sampled value is not a recorded peak; `0/0` is not a passing population.
13. **Verify a mechanism by the running thing, not its configuration.** A flag, label, declared owner,
    exit code, or queue entry may be stale or inert. Observe the live process, authoritative record, or
    exact consumer that governs the decision.
14. **Bracket both directions.** Prove that a qualifying case is accepted and a violating case is
    refused. Never plant a real approval, validation label, merge, or other authorization to test the
    refusal; use an inert fixture or isolated repository.
15. **Closed does not mean landed; landed does not mean deployed.** After publication, evidence, and
    adversarial confirmation, the owning agent closes its own task with
    `tg update <id> --status closed`; red, missing, or stale selected validation does not qualify.
    After landing, verify ancestry and activate the landed commit in the primary checkout; ancestry
    alone does not prove the running coordinator or checkout uses it. Record the evidence BEFORE you change status:
    `tg note <id> "IMPLEMENTED: <artifact-url> | SHA <40-hex> | <validation>"`, then
    `tg update <id> --tags <existing-tags>,implemented`.

Landing debt rides on the `implemented`
tag, never on the status. After landing, `./ci-hub/bin/close-task` verifies ancestry and writes
`CLOSURE-VERIFIED`. Follow the [closure evidence contract](ci-hub/closure/README.md); never infer landing
from task status.

# Canonical action guides

These files are the single sources of truth for their action. Do not restate their rules elsewhere.

- For Hermit validation authority, exact-commit evidence, hard green, soft-green rebase inheritance,
  red-main repair, and the local-versus-GitHub boundary, follow
  [hermit-validation-authority](.claude/skills/hermit-validation-authority/SKILL.md).
- For provisioning, registering, adopting, using, releasing, or reclaiming a worktree slot, follow
  [worktree-provisioning](.claude/skills/worktree-provisioning/SKILL.md).
- Before choosing or assigning a pull-request landing batch, follow
  [pr-landing-planner](agent-utils/skills/pr-landing-planner/SKILL.md). It plans and does not mutate.
- To execute an authorized landing plan and prove the resulting ancestry, follow
  [pr-landing-operations](.claude/skills/pr-landing-operations/SKILL.md).
- For critical-change classification, review count, exact-head review, audit markers, and post-land
  human review, follow [post-facto-review](.claude/skills/post-facto-review/SKILL.md).

# Repository and publication boundaries

- Product source, tests, and documentation live in their product repository. Parent policy, exact
  submodule commit pointers, orchestration, reproducible experiments, and durable reports live in
  `dev-hermit`.
- Hermit's hermetic Reverie dependency is its exact revision pin. The pin may lag live
  `rrnewton/reverie:main`; it must remain on that branch's linear ancestry and may only stay equal or
  advance relative to the Hermit base. Advance it deliberately—never treat a lag count as drift or
  synchronize it merely to the live tip. The parent repository's Hermit/Reverie/LiteInst gitlink triple
  is separate real-time product-main snapshot bookkeeping and does not redefine Hermit's dependency.
  The [Reverie pin checker](https://github.com/rrnewton/hermit/blob/main/scripts/check-reverie-pin.rs)
  implements the ancestry and monotonicity boundary.
- Ordinary Hermit work lands through a feature pull request to `rrnewton/hermit:main`; ordinary Reverie
  work targets `rrnewton/reverie:main`. Never force-push `main` or a shared branch. Direct-main Hermit
  repair is limited to the red-main procedure named by
  [hermit-validation-authority](.claude/skills/hermit-validation-authority/SKILL.md).
- Primary product checkouts remain clean on their current `main`. Mutating product work uses the
  registered checkout supplied under
  [worktree-provisioning](.claude/skills/worktree-provisioning/SKILL.md).
- Never create bot-authored issues on `facebookexperimental/hermit` or
  `facebookexperimental/reverie`; those repositories feed an internal task tracker. Use the `rrnewton`
  fork through [the registered issue wrapper](.orc/plugins/hermit-dev/gh-issue-create).
- Never commit binaries, build output, credentials, caches, database dumps, profiler captures, or other
  generated artifacts. A large external artifact needs a small versioned text manifest naming its
  location, checksum, producing command, tool version, and source SHA.

# Disclosure and evidence

Before inserting the disclosure in a commit subject or pull-request description, run
`./ci-hub/bin/who-am-i --tag --role <artifact-role>` from the `dev-hermit` parent and paste its exact output
at the start. Do not type, reconstruct, or normalize the output; the command resolves the caller and refuses
when it cannot. GitHub comments use the [registered boundary](ci-hub/bin/gh), which validates the caller
context and prepends the disclosure, so their bodies remain unprefixed.
The prefix is disclosure, not authentication, and never proves reviewer independence. Install the
[repository hooks](scripts/setup-hooks.sh) before committing. Immediately after the subject/body, record
the stable task with `Task: <task-id>` when the commit implements task work.

Every implementation handoff names the artifact URL, repository, branch, exact 40-hex commit, tested base,
exact commands and results, limitations, and cross-repository dependency commits. Every quantitative report
follows the [communication-precision rules](ai_docs/agents-md-policy-rationale.md#communication-precision--full-report-writing-rules).

# Named rationale and history

Read rationale only when the decision needs it; operative action stays in the skills above.

- [Proxy Binding rationale and authority registry](ai_docs/agents-md-policy-rationale.md#proxy-binding-review-axis--full-rationale)
- [Establish-what-you-have rationale](ai_docs/agents-md-policy-rationale.md#establish-what-you-have-before-acting-on-it--full-rationale)
- [Running-mechanism rationale](ai_docs/agents-md-policy-rationale.md#verify-a-mechanism-by-the-running-thing-not-its-config--full-rationale)
- [Process-kill incident history](ai_docs/agents-md-policy-rationale.md#process-kill-safety--war-story)
- [Task-closure rationale](ai_docs/agents-md-policy-rationale.md#task-lifecycle--phantom-closure-rationale)

# Current temporary directive

Owner directive, 2026-08-11: hourly status and alignment interrupts are suspended because they distract
from the highest-priority work. The review trigger is the owner revisiting those mechanisms. Until then,
do not run or claim that `hermit-hourly-status.timer` or `alignment-reminder-relay.timer` reloaded policy;
preserve `ai_docs/status-log/status-log.jsonl`. Do not silently extend the suspension after the review
trigger.

<!-- LOAD-VERIFICATION TAIL CANARY. Keep this as the final line. -->
**TAIL-CANARY-KESTREL-7731**
