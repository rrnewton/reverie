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
generalised from* until it recurred on a different channel. Read back what you
posted, on every channel, not just the exit code.

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

### A load-bearing figure must be traceable to a producer artifact

**[recorded]** Of 13 load-bearing figures in one lane's reporting, **1** could be
traced to a producer artifact; the rest lived only in notes. A separate finding
the same night — 18 named cells living in a note rather than a file anyone could
diff or script against — is the same defect. An audit cannot distinguish a number
that lives only in prose from a number that was never produced.

### A count of things beats a measurement of seconds when the question is whether something changed

**[recorded]** A ~50-second swing between two otherwise identical runs made every
single-run CPU-seconds figure uninterpretable. The discrete probe count
`7 -> 8` at `91533e73` survived the same noise and answered the question.

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

**[recorded]** Recorded against a verify-allow certification path. The verdict
channel has to observe the property being asserted, not a proxy that the
violation leaves untouched.

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
night.

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

### Do not re-run or regenerate a missing or truncated artifact — report it where it sits

**[measured]** Re-running substitutes a fresh artifact for a lost one and destroys
the evidence that anything drifted. A relocated file is evidence of the fault and
should be catalogued in place. Applied during the artifact audit at the end of the
session: the two failing checksums were reported, not refreshed.

---

## Working with agents

### Give the agent a kill condition it can report against you, and say that reporting it is the successful outcome

**[recorded]** Without this, an agent asked to find something finds something.

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

### A rule without its incident becomes a ritual

**[recorded]** Stated during the session as: *if we started requiring 100 runs of
`arch_prctl` we would be performing rigour rather than exercising it.* The
requirement that produced a rule is only defensible while the failure it answers is
attached to it. This file exists to keep those attached.
