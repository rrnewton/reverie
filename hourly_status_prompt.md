<!--
Wake prompt for the durable hourly owner-status driver.

Delivered by scripts/hourly-status-relay.rs, which is fired by
hermit-hourly-status.timer (systemd --user). The driver prepends a machine-
generated header naming the exact scheduled hour and the paths involved, then
appends this file verbatim. HTML comments are stripped by
scripts/orc-hermit-msg.py before delivery, so this block is not sent.

Edit this file to change WHAT the coordinator is asked to do each hour. Do not
put scheduling logic here -- the driver owns the schedule and the per-hour
deduplication.
-->

HOURLY OWNER STATUS IS DUE.

This prompt is for the ORC coordinator. Do not delegate Google Chat delivery to
a worker. Workers return evidence through verified TaskGraph notes or tracked
`ai_docs` reports; the coordinator alone synthesizes and sends the owner-facing
message with `orc.sendGchat`.

Do all four steps, in order. Steps 3 and 4 are the ones that have silently
failed before, so do not stop after sending.

**1. Check the hour first — do not double-send.**
The header above names the SCHEDULED HOUR this wake belongs to. If a status for
that same hour has already gone out by any other path (notably the legacy
in-session `hourly-status-report` workflow, which fires on its own offset and
which this driver is meant to replace), do NOT send a second one. Skip to step 4
and record the skip. One status per scheduled hour, whoever sent it.

**2. Synthesize one status and send it to the owner GChat space.**
Space `spaces/AAQAA6Irlwg`. Cover: fleet size and per-agent activity;
egress/outage state; what actually tightened this hour; and blockers needing
owner input. Synthesize — do not dump a task list. Lead with the observable
consequence and the decision it creates. If nothing material changed, send a
one-line steady-state note rather than skipping: a skipped hour is
indistinguishable from a dead driver, which is the failure this whole mechanism
exists to prevent.

**ONE BULLET PER AGENT, AND IT NAMES WHAT THAT AGENT IS DOING RIGHT NOW.**
The 2026-08-07T01:00 report listed 19 workstreams for 7 agents — `hermit-w3`
appeared seven times — and not one of the 19 was active work: 16 were tagged
`implemented` and 3 had not been started. It rendered an ownership ledger as an
activity report, and the owner could not tell whether `hermit-w3` was an agent
or a workstream. So, per live agent:

- **Exactly one bullet.** An agent is a worker, not a portfolio. Its one bullet
  is the single task it is working on this hour.
- **Derive activity from live TaskGraph, not from memory or from ownership.** A
  task tagged `implemented` is finished and waiting to land — a landing
  obligation, not activity. Since 2026-08-06 such a task is `closed` +
  `implemented`, and the pending-landing set is tracked in the single open task
  `drain-implemented-to-landed`. `open`/`backlog` means not started. None of
  these belong in the active list, and a `closed` + `implemented` task is
  **not** idle or stuck — it has no owner by design, so flagging it recreates
  exactly the noise this rule removed.
- **Name the workstream, not the id.** Use a stable descriptive
  `major-goal/sub-goal` slug — `backend-parity/dbi-stack-hash-determinism`, not
  `dbi_detlog_stack_hashes` and never `phase-1` or `option-a`. The same work
  keeps the same slug next hour, so the owner can follow it across reports.
- **Say where.** Give the agent's slot or working directory, so a name resolves
  to a workspace.
- If an agent has no active task, say that plainly. "Idle, 2 PRs awaiting
  landing" is a real and useful status; seven stale bullets are not.

**3. Append the structured record with the EXACT text you delivered.**
Run `scripts/status-log.rs`. The `status_text` you log must be byte-identical to
what GChat actually received — take it from the send call's response, do not
retype or re-summarize it. The mapping mirrors the rules above and the script
enforces them: one entry per agent, `major-goal/sub-goal` slugs, a mandatory
`cwd`, and every task id dereferenced against live TaskGraph. It refuses rather
than records a mapping that disagrees, so a refusal means the report needs
fixing, not that the flag needs working around.

```
./scripts/status-log.rs \
  --mapping-json '{"<major-goal>/<sub-goal>":{"agent":"<agent>","task":"<task-id>","cwd":"<slot-or-cwd>"}}' \
  --awaiting-landing-json '{"<major-goal>/<sub-goal>":{"agent":"<agent>","task":"<implemented-task-id>","cwd":"<slot-or-cwd>"}}' \
  --status-file <file containing the exact delivered text> \
  --covers-hour <YYYY-MM-DDTHH the report DESCRIBES> \
  --expect-sha256 <sha256 of the API response text> \
  --expect-bytes <byte count of the API response text> \
  --repos <owner/name>[,<owner/name>...] \
  --open-prs auto --genuine-reds <N> --fleet-count <N> \
  [--ready-prs <N>] \
  [--recovery-for <YYYY-MM-DDTHH>]
```

`--expect-sha256` and `--expect-bytes` come from the **send response**, not from
the file you happen to have on disk. They are what binds the logged text to the
delivered one: on a mismatch the script REFUSES the append rather than recording
a status that is not the one the owner received. Take both from the API record:

```
messageName / space / thread / text   <- the sendGchat response
sha256 = sha256 of that response's exact `text` field
bytes  = its byte length
```

If they disagree with your status file, the file is wrong — most often by one
trailing newline, because every editor and heredoc terminates the last line and
the sent message did not. `status-log.rs` strips at most one trailing terminator
so a normal file still matches; a *second* newline, or any edit after sending,
will and should be refused.

`--covers-hour` is the UTC hour the narrative and counts DESCRIBE, which is not
always the hour you are writing in. Pass the hour from the header. If you are
writing late — recovering an hour whose scheduled delivery was missed — you must
ALSO pass `--recovery-for <same hour>`; the script refuses a cross-hour write
that does not declare itself. One entry once carried 9 p.m. counts, a 10:28 p.m.
mapping and a 10:28 p.m. timestamp with nothing in the row to tell them apart.

`--repos` is REQUIRED and names the repository set every count below was taken
over — it is the denominator, and the script refuses without it. Name the repos
explicitly rather than relying on "the usual ones"; an unstated denominator is
what once put `open_prs=105` and `open_prs=10` in the log nineteen minutes
apart, one counting all open PRs and the other a ready subset.

`--awaiting-landing-json` is optional and is where implemented-but-unlanded work
goes: it stays on the record without being counted as activity.

Counts travel with their denominators: `open_prs` is TOTAL open INCLUDING
DRAFTS across exactly the repositories you passed to `--repos`, and
`--genuine-reds` comes from `./ci-hub/ci-hub pr-status`, not from a glance at
GitHub. If you also report the ready/non-draft subset, it goes in `--ready-prs`;
it is a separate field and can no longer be written as `open_prs`.

**Pass `--open-prs auto` rather than counting yourself.** Auto dereferences
exactly the repositories in `--repos`, once, and adopts that total — recording
`source=dereferenced-auto`, the per-repo breakdown, and the window the lookup
spanned. Counting in your head and passing a number is a race you cannot win:
the writer re-derives it to check your claim, and a population that moves
between the two readings is refused even though both readings were honest. That
happened for real — a status was refused at 104 against an observed 105, and the
count moved 104 → 105 → 106 over the following minutes.

You may still pass an explicit `--open-prs <N>` when you genuinely mean to
assert a number, and it is still CHECKED against the population and refused on
mismatch. `auto` is the only value that is adopted. Do not combine `auto` with
`--open-prs-observed` or `--no-verify-counts`; each would record a total whose
stated provenance is untrue, and all three combinations are refused.

Note that `ci-hub pr-status` polls TWO repositories (`rrnewton/hermit`,
`rrnewton/reverie`) and reports its MONITORED ROLLUP — the ready/non-draft PRs
it watches, a handful, not the hundred-odd total. That rollup is NOT `open_prs`:
an entry once recorded 7 under the total-open basis while the true total was
102. Take `--genuine-reds` from pr-status; let `auto` supply the total; and if
you report the monitored subset at all, say so in the narrative rather than in a
count field that means something else.

**4. Report the outcome back into the hour's record — by ATOMIC TYPED REWRITE,
never by appending.**
The driver has already written a per-hour claim file; its path is in the header.
That file is how a later investigator tells "the hour was handled" apart from
"the driver fired and the coordinator dropped it on the floor".

**It is JSON, and it is the dedupe authority. Do not append a line to it.**
`read_claim` parses it with `serde_json`, which rejects trailing content, so one
appended human-readable line makes the whole claim unparseable. The driver then
fails closed — `outcome=corrupt-claim` — and that hour can no longer be
processed until a human repairs the file by hand. (Before the fail-closed fix
landed, the same append was worse: the claim degraded to a synthetic
`pending@epoch0`, aged into `ReclaimStale`, and RE-SENT an hour that had already
gone out.)

**Record the acknowledgement with the driver, not by hand.** The relay owns the
claim schema and rewrites it atomically for you:

```
./scripts/hourly-status-relay.rs \
  --ack-hour <this hour> \
  --ack-message-name <spaces/<space>/messages/<id> from the send response> \
  --ack-space <spaces/<space>> \
  --ack-thread <spaces/<space>/threads/<id>> \
  --ack-text-file <file holding the EXACT delivered text>
```

All five are required together, and the digest is computed from
`--ack-text-file` rather than taken on your word. The three identifiers must
name ONE conversation — the space has to prefix both the message and the thread
— so three well-shaped unrelated strings are refused. This is the same canonical
text rule `status-log.rs` uses, so the claim's `gchat_text_sha256` and the log's
`status_text_sha256` come out equal; an auditor compares exactly those two.

**Do not hand-edit the claim.** It is JSON and the dedupe authority; the typed
rewrite above is atomic and validated, a Python snippet is neither.

**What the states mean now.** The driver writes `wake_accepted` when the relay
typed the wake into your pane — that is stage one and it is NOT a delivery.
Only `--ack-hour` promotes the hour to `gchat_delivered`, which is what closes
it. An hour left at `wake_accepted` is re-woken (bounded, three attempts) and
then reported as `gchat-ack-missing`: an unmet obligation, deliberately not a
success. So running the ack is not bookkeeping you can skip — it is the only
thing that tells the system the owner actually received the status.

(`delivered` is the pre-split legacy state. It still dedupes, so historical
hours stay closed, but nothing writes it any more. Earlier revisions of this
prompt said never to set `gchat_delivered`; that advice predates the two-stage
split and is now exactly backwards.)

Afterwards the claim must still parse and still dedupe. Confirm with a dry run,
which writes nothing:

```
./scripts/hourly-status-relay.rs --hour <this hour> --dry-run
```

After a successful ack it must print `outcome=duplicate-hour ... claimed=no-op`.
If you have woken but not yet acked, `outcome=awaiting-gchat-ack ... delivered=no`
is correct and means the ack above is still owed. `outcome=corrupt-claim` means
the file is not valid JSON — fix it rather than leaving the hour wedged.
