# Operating rules from the 2026-08-15 overnight session, each with the incident that produced it

Status: durable record of rules derived from real failures. Date: 2026-08-15.

Every rule below was produced by a specific failure during the overnight session
of 2026-08-15 on `devbig030` and `devbig014`. **A rule without its incident
becomes a ritual within a day**, so each entry names its case. Any candidate rule
that could not be attached to a concrete incident from that session was left out.

Filename note: this file uses a datestamp-*first* name as the commissioning task
required. The existing convention in this directory is datestamp-last
(`subject_20260803.md`). The deviation is deliberate, not an oversight.

Provenance is marked per rule:

- **[measured]** — the author of this file produced and verified the incident directly.
- **[recorded]** — the incident is recorded in another agent's task notes or in the
  commissioning task description; not independently re-verified here.

Rules are grouped only so related ones sit together. **The order carries no
ranking.**

---

## Reporting and evidence

### `rc 0` means the API stored what it received, not that it received what you sent

**[measured]** A GitHub PR comment on #2269 was posted with backticks inside a
double-quoted shell argument. Command substitution silently ate the identifier
`write_syscall_context`, leaving `One design note:  now takes the options`. The
post returned success. It was caught only by reading the comment back through
the API and grepping for the word, then repaired with a quoted heredoc.

The identical class had already been recorded for `tg note` bodies earlier the
same night — plain `<<EOF` expands, only `<<'EOF'` is safe — and *was still not
generalised from* until it recurred on a different channel.

A third instance the same session, on a different mechanism: a backgrounded
`tg show | grep` finished with **exit code 0 and produced no data**, because the
regex hit ugrep's complexity limit and both patterns refused to run. The task
reported "completed (exit code 0)". Nothing had been searched. **A filter that
declines to run and a filter that finds nothing are the same exit code and, at a
glance, the same empty output.**

Read back what you posted or retrieved, on every channel, not just the exit code.

### When a filtered run shows fewer lines than expected, the missing lines are the finding

**[measured]** A new unit test guarding a best-effort memory read was checked
with `cargo test … | grep -E "^test |test result"`. The test binary was
**segfaulting**, because `LocalMemory` reads the current process's own memory and
a deliberately bad pointer faults rather than returning `Err`. The segfault line
matched neither grep pattern, so a crashing test rendered as a short, clean-looking
list of passes. It was caught only by running a mutation and noticing the *mutant*
also "passed". The test was deleted, not repaired: the policy it aimed at is not
unit-testable with `LocalMemory` and is covered end-to-end instead.

A filter that returns something looks identical to a filter that returns
everything. Count what you expected.

### Re-verify your own published checksums before citing them

**[measured]** A `SHA256SUMS` manifest was written at 21:37:05 naming the
measurement instrument `revents_probe.c` as `37b56dbd…`. The instrument was then
edited at 21:44:46 to add a mode needed for a later experiment, and the manifest
was never refreshed. The stale hash had already been cited as the instrument's
identity in a handoff task filed for another agent. **The version the hash names
was never preserved and is gone.** Six of eight artifacts still verified; the two
that failed were the ones the author had touched most recently.

### An audit that reports zero negatives over a population known to contain them is reporting its own binding, not the population

**[recorded]** `ancestry_audit.py` reported **58 LANDED / 0 NOT-LANDED while 17
pull requests were still open**. Acting on it would have phantom-closed 58 tasks
and undone the same day's closure fix. The audit bound "landed" to any associated
SHA, so every task with a SHA attached satisfied it.

A zero in the refusal column is not a clean bill of health; it is the first thing
to disbelieve. Ask what input would have produced a negative, and check that such
an input exists in the population.

After the fix — admit a candidate only if a linked PR's own `mergeCommit` or a
commit carrying `Task: <task-id>` binds it to the task — the same population moved
**34 LANDED→LANDED, 13 LANDED→NOT-LANDED, 11 LANDED→NOT-DETERMINABLE, 1
UNKNOWN→NOT-DETERMINABLE**. The 0 was never the population's shape; it was the
binding's.

### Before tightening a check, measure how much of the population the tightened rule keeps

**[recorded]** From the same repair, in the fixing agent's words: *"I measured
before choosing the rule, so the audit would not be gutted: of the 58, 19 had a
linked-PR mergeCommit and 15 had a commit naming the task. A sound rule keeps 34+,
not 0."*

A check that produces false positives is fixed by making it stricter, and the
obvious failure is overshooting into a check that refuses everything. Both
directions are wrong and only a measurement distinguishes them. This is the
accept-case discipline applied to a gate's own tightening.

### When candidates exist but none is attributable, the honest verdict is a third one

**[recorded]** The repaired audit added a `NOT-DETERMINABLE` bucket — *"never
LANDED, never NOT-LANDED"* — plus a `refused` field listing what was inspected and
why it did not count. Eleven of the 58 landed there.

Forcing an unattributable case into pass or fail manufactures a claim. Note-quoted
SHAs, merge commits of incidentally-mentioned PRs, and SHAs tested against a
repository they did not come from are non-evidence; recording them as *refused*
keeps the audit trail without letting them vote.

### A record in a coordination tool is not the authority; check the channel that actually binds

**[recorded]** An approval recorded in TaskGraph **did not exist in the canonical
binding on the pull request.** The lane had done the review; the authority channel
had never received it. Adjacent and measured the same night: `approval_binding
--gate` inspects labels, not thread comments, so an un-attested `APPROVED-AT` in a
PR thread reads as CRITICAL-REVIEW-MISSING.

### The practice we require can be the thing that creates the defect

**[recorded]** Agents are required to cite PR URLs, branches and head SHAs in task
notes. `pile()` flattens all task-note prose, so **an unrelated quoted PR URL plus
a branch name or a 12-character head is enough to attribute work to the wrong
task.** The citation discipline that makes findings auditable is what feeds the
misattribution.

When a tool consumes prose that a policy requires agents to write, the policy and
the parser are one system. Check that the parser can distinguish a citation from a
quotation before requiring more citations.

### A load-bearing figure must be traceable to a producer artifact

**[recorded]** Of 13 load-bearing figures in one lane's reporting, **1** could be
traced to a producer artifact; the rest lived only in notes. A separate finding
the same night — 18 named cells living in a note rather than a file anyone could
diff or script against — is the same defect. An audit cannot distinguish a number
that lives only in prose from a number that was never produced.

### A count of things beats a measurement of seconds when the question is whether something changed

**[recorded]** The same-revision noise band was measured directly: four arms at a
fixed head `233c0f144` spread **+71.94 CPU-seconds, +43.5%**. That made every
single-run cost figure of the night uninterpretable, including ones already
reported.

**Three times a count outlived a duration in the same session**: the probe count
`7 -> 8` at `91533e73`, the `336 tests / 5376 cells` population, and `55` die-sites.
When the question is *did this change*, choose the discrete observable.

---

## Tests, gates and verification

### A test that shares a derivation with the thing it checks cannot fail — construct the mutation and run it

**[measured]** A test asserting that a verification report carries
`virtualize_time` constructed its own options with `virtualize_time: false` and
asserted the report echoed it. It therefore tested that a struct field
round-trips, and **passed under a mutant that made the record path claim time
*was* virtualized**. It never bound the call site at all.

The repair was not a better assertion. The duplicated literal was deleted so the
report reads the configuration the run actually used, and the pin was moved onto
the configuration itself.

### Prove the mutation took effect, then prove the gate caught it

**[recorded]** A remediation was observed rejecting its own mutation *because the
mutation never applied*. Two separate facts are needed: that the mutant is really
present in the artifact under test, and that the gate then refuses it.

### An accept case and a refuse case, together — the accept case is the one that catches the damage

**[measured]** A fix that captured a partially completed `ppoll` copy-out on
`EFAULT` flipped its target case from diverged to matched, which looked correct.
It also read guest memory on *every* error return, so on `review-cases
invalid-nfds` — which passes `fds=(void*)1` with `nfds` over `RLIMIT_NOFILE` — the
recorder's own `EFAULT(14)` replaced the kernel's `EINVAL(22)` and flipped a green
cell red.

**Without the accept case the change looks correct and silently corrupts a
recorded errno.** A reviewer shown only the flip would have approved it.

### Ask what external fact the gate checks against; nothing outside itself means it cannot detect its own weakening

**[recorded]** This discriminator identified self-certifying gates that the
derived-versus-fixed heuristic missed in five of seven cases. The heuristic was
the plausible test and it was the wrong one.

### A guest whose contract violation does not change its exit status cannot be refused by a verdict that keys on exit status

**[recorded]** Case: startup-surface-identity. The accept arm was sound — 5/5
matched with bitwise parity. The mutant was **proved to bite**: `ENVCOUNT 121 vs
122`, with both binaries confirmed to have run. **And the mutant still exited 0.**

So the gate was well-formed, the mutation was real, and the verdict could not see
it, because the channel it keys on is one the violation does not disturb. The
verdict has to observe the property being asserted, not a proxy.

### Re-measure on the head that will ship, not the head you measured an hour ago

**[measured]** A scope gap — that a ppoll fix left `poll` untouched — was first
measured on a prototype against `origin/main`. Re-measured on the actual folded
head, `poll` still diverged (record `revents0=1` vs replay `revents0=4660`),
confirming the gap against shipping code. In the same pass, the *target* case went
*further* on the folded head than it had on the prototype, because an intervening
change had fixed a second divergence. Inheriting the earlier result would have
understated the outcome.

---

## Interpreting agreement, absence and noise

### Two careful readers agreeing about one shared artifact is one observation, not two

**[measured]** Two reviewers on two machines independently read the `run 1` /
`run 2` identifiers in a shared comparator and concluded that
`record start --verify` executes the guest twice. It does not — it executes once
and replays. Both coordinators treated the agreement as independent confirmation,
a correct finding was retracted across two machines and to the owner, and a P0 was
re-scoped. It was one misleading artifact reported twice.

Ask what each reader actually looked at before counting them as two.

### The question is not "is this true" but "is this true of what"

**[recorded]** Eight scope errors across two hosts in one night, each defensible
on its evidence and wrong in its quantifier. One **[measured]** instance from this
session: the claim that a raw-`ppoll` timeout leak was broadly reachable, corrected
to *raw-syscall callers only* after checking that glibc's `ppoll` wrapper shields
its caller and that x86_64 glibc `poll` does not compile to `ppoll`. The
measurements were never in question; the population they applied to was overstated.

### "Not found by this search, and here is what I searched" — never "does not exist"

**[measured]** A search for the rationale behind `virtualize_time: false` covered
`git log -L`, the product tree, `docs/`, and GitHub issues, and found nothing. The
conclusion drawn was "undocumented". The rationale existed the whole time in the
*parent* repository's `ai_docs`. The absence was real and was itself the finding —
but it was an absence *in the place searched*, not in the world.

### A stack of blockers reports only its top element

**[recorded]** Check whether the *error changed*, not whether the command still
fails. A second blocker behind the first is invisible until the first clears.

### Do not report a flake as a regression

**[measured]** During a probe, `/bin/echo hello` came back `diverged` and looked
exactly like a regression caused by the change under test. It did not reproduce:
8/8 matched under the probe and 8/8 under the restored baseline. One divergence in
21 runs, attributable to neither arm. Reporting it would have manufactured a reason
where none existed — and it was the *only* apparent evidence that the change was
load-bearing.

### Test the mechanism, not just the correlation

**[measured]** A plausible hypothesis held that a byte-count divergence
(94 vs 97) was timing-derived, since record mode admits real elapsed time. It was
refuted two ways: the divergence was bit-stable 10/10, and the only timing-varying
field in that line is printed with `%09ld`, which pins its width — five native runs
gave five different nanosecond values and a 94-byte line every time. A varying
value in a width-pinned field cannot change a byte count.

---

## Where knowledge lives

### Put the rationale at the code; a decision recorded in another repository is operationally undocumented

**[measured]** `record_or_replay_config` sets `virtualize_time: false`. The
rationale — that record mode sits on the record side of a designed
determinize-versus-record boundary — was documented in the *parent* workspace's
`ai_docs`, together with a ~1034-line implementation attempt (#586, closed by
directive to recreate against #662, which merged). **Three agents read the flag and
could not tell a design position from a defect; two coordinators then spent hours
on it.** The product repository contained no comment, no doc and no issue.

The repair was a doc comment at the definition itself, stating the position, the
mechanism, and the consequence that two recordings of one guest need not agree.

### A label that is true on one path and false on another is worse than no label

**[measured]** `run 1` / `run 2` is shared comparator text. On
`hermit run --verify` it is accurate — two fresh executions genuinely occur. On
`hermit record start --verify` the same strings describe a recording and its
replay. Nothing in the message told a reader which path produced it. That is what
caused the double misreading above.

The fix was path-specific, not a global rename: each compared side now names
itself, and the run path's wording is unchanged, because renaming globally would
have been just as wrong in the other direction.

### End the defect class by deleting the duplicated source of truth, not by correcting its value

**[measured]** Applied three times in one night.

1. A record-mode call site restated `virtualize_time: false` as a literal beside
   the config that already held it. Correcting the literal would have left two
   copies free to drift; reading `record_or_replay_config(data_dir).virtualize_time`
   means report and run **cannot disagree by construction**.
2. `ComparedRun.label` was given **no default**, so the compiler now refuses any
   new comparison path that does not name its two sides.
3. A helper taking three values derived from one options struct was changed to take
   the struct. Three derived arguments are three chances for printed names to drift
   from the compared streams — the same duplication, one layer down. (It also
   cleared a `clippy::too_many_arguments` error without an `allow`.)

### Verify by running the thing, not by grepping the source

**[measured]** After fixing the four mislabelled sites a source grep had
identified, actually running a diverging record/replay exposed **three more** — the
retained-log listing labelled the recording's log `run 1`, which is the most
consequential of all because it is the path a reader opens next. The filenames
beside it were already correct, which is what made the wrong labels effective.

---

## Infrastructure faults

### An infrastructure fault must report infra, never a cell verdict

**[recorded]** A transient EdenFS `ENOTCONN` (os error 107) on an `fbsource`
`PATH` entry makes `/usr/bin/env bash` fail `rc=126`, **which reads as every shell
fixture refusing**. Under `ENOTCONN` on a `PATH` entry, absolute paths do not help
— `/usr/bin/env` itself cannot resolve `bash` — and an unchanged retry succeeds.
The same error code took out tmux, the `meta` CLI, and script spawning the same
night. **Absent a gate that classifies this as infrastructure, it reads as every
shell fixture refusing** — a whole-population red produced by one disconnected
filesystem.

### A universal failure cannot separate the work from the environment — only a second host can

**[recorded]** `gate.manifest` failed on **every** local run for twelve hours,
voiding local receipts. devbig014 ran the identical head `2b9f1de0ca2c` and **it
cleared** — no 53-planned / 2-passed / 50-skipped, no NOT-VALIDATED. The fault was
the host, not the work.

A failure with no passing case in view carries no information about its own cause.
The discriminator is not more runs on the same box; it is one run somewhere else.

### An uncommitted edit to a shared hook under `core.hooksPath` is a fleet-wide outage, not one lane's work in progress

**[measured]** `git config core.hooksPath` is `.githooks`, so git executes the
**working-tree** copy of a hook, not the committed one. During this session the
working-tree `.githooks/pre-push` was modified — uncommitted — to exec
`scripts/main_write.py`, which does not exist at that path and was never committed
there; the file lives at `agent-utils/scripts/main_write.py`. Every push to parent
main from this checkout failed.

**It then deadlocked writes entirely.** `parent-main-write commit` refuses unless
local main equals freshly fetched `origin/main`; an unpushable local commit breaks
that; the push that would clear it is the blocked one; and plain `git commit` is
independently refused with "missing serialized-writer lock receipt". The first
agent to attempt a parent-main write after the edit appeared gets stuck and stays
stuck, with no sanctioned path out from inside the checkout.

An uncommitted change to a versioned hook is not private to the lane making it. It
is already live for everyone sharing the checkout.

*Attribution of the edit is open at time of writing and routed to the lane owning
serialized parent-repo writes. The mechanism above is measured; the ownership is
not.*

### A write that returns success can still have been truncated

**[recorded]** An overnight report was truncated to **41 lines from ~204 with the
append reporting success**. A noise-band experiment died with arm 1 at `rc=2`
truncated and arms 2–4 at `rc=126` "cannot execute", with the worktree and script
intact and executable.

Two candidate causes were under investigation concurrently: a `cwd`-drift fault (a
driver script `cd`-ing relative to an unstable base) and the `ENOTCONN` fault
above. **They overlap and were explicitly recorded as needing separate
confirmation rather than assumption.** Recorded here as an open attribution, not a
settled one.

### Persisted to an untracked path is not persisted

**[measured]** Three artifacts were written under `ignored/` during the session
specifically to rescue findings from agent-context loss. **`ignored/` is
gitignored.** They survive context loss and die with the host.

This applies to the present author's own evidence: the measurement instruments,
prototype patches and run directories cited throughout the session all live under
`ignored/revents-measure/`. They are durable against one failure mode and not the
other. **Durable-against-context and durable-against-host are different
properties, and only the second survives the machine.**

### Do not re-run or regenerate a missing or truncated artifact — report it where it sits

**[measured]** Re-running substitutes a fresh artifact for a lost one and destroys
the evidence that anything drifted. A relocated file is evidence of the fault and
should be catalogued in place. Applied during the artifact audit at the end of the
session: the two failing checksums were reported, not refreshed.

---

## Working with agents

### Give the agent a kill condition it can report against you, and say that reporting it is the successful outcome

**[recorded]** Without this, an agent asked to find something finds something.

### When one half of a sentence in a handoff note is disproved, the other half is not independent evidence

**[recorded]** Raised by hermit-bytes on the third stale blocker of the night. A
compound claim written by one agent in one moment shares one act of observation.
Refuting part of it does not leave the remainder standing on its own footing — it
leaves the whole sentence needing re-verification.

### A note is one agent's unverified belief; verify the premise before acting on it

**[measured]** A design document asserting that a configuration choice was
deliberate was itself agent-authored and *quoted* an owner directive. The quote was
verified verbatim at source before being relied on. It held.

The same check surfaced a ceiling worth knowing: **every commit in the Hermit
repository is authored "Ryan Newton"** across five email addresses, one of which is
the reserved fixture domain `example.com`, and the disclosure prefix is explicitly
"disclosure, not authentication". **No repository artifact can distinguish
human-owner intent from coordinator-agent intent** — for this or anything else. If
human sign-off is required, it must come from the owner directly.

### Knowing the pattern does not prevent the error; only checking does

**[recorded]** The coordinator relayed a bounded-search result — "1 of 13 figures
traceable" — to the owner as a verdict, **one hour after telling another agent that
"not found by this search" is only useful if the search is named.** The expanded
search then recovered the seven cells, `96.835%`, and `56`.

This is the most repeated error of the session and it was made by the participant
who had just articulated the rule against it. Treat familiarity with a failure mode
as no defence against it. The control is the check, performed, every time — not the
knowledge that the check exists.

### A machine check catches what knowing the lesson does not

**[measured, n=3]** Three times in one session a mechanical check caught a defect
the agent had *already diagnosed* and walked into anyway.

1. **Prefix collision, caught by the manifest validator.** hermit-sabre's edit
   sliced the verify block at the next `\n[test.`, which also matches
   `[test.modes.verify.backends_disabled]`. The validator refused it —
   `modes.verify must partition` — **one hour after sabre had named that exact
   trap out loud.**
2. **Stale local ref, caught by the serialized writer.** `parent-main-write:
   REFUSED: local main is not the freshly fetched origin/main`, local
   `1a913eaf` against origin `1130fe9c`. That is **the same stale-ref condition
   that had produced a false absence earlier the same night**, by the same agent,
   who had already diagnosed it and written it up.
3. **Non-firing replacement, caught by a sanity assert.** Cells carrying
   `backends_enabled = ["ptrace", "dbt", "sabre"]` meant a literal `["ptrace"]`
   match never fired, and the edit would have removed `liteinst` from disabled
   without adding it to enabled — **the inverse of sabre's own earlier partition
   bug, caught by the guard it had built from that bug.**

**The contrast is the argument.** Seven matcher failures in the same session were
caught by a person noticing an implausible result — that is, *after the wrong
answer already existed*. These three were caught before anything was written.
In all three the agent knew the lesson; knowing it did not help and the check did.

**AND THERE IS A CONTROLLED CASE, WHICH IS WORTH MORE THAN THE THREE ANECDOTES.**
Inside a single PR (#2279), describing a single change, by a single author, on the
same night: **the cell counter was exact at 14/14/14/+14 across four independent
readings — manifest, SCORECARD green, plan total, plan liteinst entries — while
the PR title was stale by six, still saying "eight".** The author's diagnosis,
verbatim: *"the counter had an oracle — `scorecard.rs update`, checked every
amend, exact each time. The title had none. That asymmetry is the finding, not
the six."*

Care, competence, attention and stakes are held constant across those two fields.
**The only variable is whether anything checked.** That is why the three catches
above are an argument rather than four anecdotes: they show a guard succeeding
where knowledge failed, and this shows the same author succeeding and failing
simultaneously, split exactly along the guarded/unguarded line.

Note also why neither stale title surfaced on its own: **both understated their
content.** #2277 said 31 for 32; #2279 said 8 for 14. A label that claims less
than it delivers attracts no complaint, so the benign direction is what let both
persist — the same reason an inert green is harder to notice than a false red.

**And the repair generalises: the count was DELETED from the title rather than
corrected**, because a corrected count goes stale on the next amend, which is
precisely how it reached six out of date. **A field that cannot go stale beats a
field that is currently accurate.** Same move as deleting a duplicated literal so
two readers cannot disagree by construction.

**AND THE SECOND HALF, WHICH IS WHAT MAKES THIS USABLE WHERE NO CHECKER EXISTS.**
"Unchecked" does not by itself predict drift. Commit messages on the same two PRs
are hand-typed, have no oracle, and did not go wrong. The author's account:
*"commit messages describe a moment rather than a state. A claim pinned to a SHA
can go stale-but-true, while a claim about THIS PR goes stale-and-false."*
Same words, same author: `adds 8 cells` in a commit message stays true at
fourteen; `eight cells` in a PR title is false at fourteen.

**So the design rule where a checker is unavailable is: ANCHOR THE CLAIM TO A
MOMENT.** A dated or SHA-pinned statement degrades into history; an unqualified
present-tense statement degrades into a lie. That is available for prose, which
is most of what we write and most of what can never have an oracle.

**The commit-message case is evidence rather than a caveat, because it is the one
that could have refuted the rule** — unchecked and yet not wrong. A hypothesis
that survives its own best counterexample is worth more than one never tested
against it, and it sharpens the claim: the predictor is not "checked" versus
"unchecked" but "anchored" versus "present-tense".

**n=3 is not a rate, and this does not claim machine checks outperform attention
in general.** What is claimed is only what was observed, plus the asymmetry that
makes it matter: **a human catch requires a wrong result to exist first, and some
wrong results are writes.** A noticed-afterwards catch on a read costs a re-run;
the same catch on a manifest edit or a `main` commit costs a repair.

**The author's own statement of why case 3 never reached the shipped state, and it
is the thesis in one sentence:** *"the guard is in the code path, not in my memory
of it — it refused the `["ptrace", "dbt", "sabre"]` case in the same pass I wrote
it."* A rule an agent holds is available only when the agent thinks to apply it. A
rule in the code path is applied whether or not anyone remembers it exists.

**Second-order, and the reason case 3 does not recur:** sabre fixed it by
generalising the match rather than special-casing the three cells it found, so
the next fixture with an unexpected backend list will not need this rediscovered.
A guard that encodes the shape outlives a guard that encodes the instances.

### A rule without its incident becomes a ritual

**[recorded]** Stated during the session as: *if we started requiring 100 runs of
`arch_prctl` we would be performing rigour rather than exercising it.* The
requirement that produced a rule is only defensible while the failure it answers is
attached to it. This file exists to keep those attached.
