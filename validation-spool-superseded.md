# cad0fd01 is superseded — do not land it

Written 2026-09-05 by agent(review-cpuid), a claude lane, after the owner re-routed
the blocked push here. Companion to `validation-spool-policy-refusals.md`.

**The re-route worked; the commit is what is wrong now.** `with-proxy` plus
`ci-hub/bin/git-push-verified` reached GitHub with no policy refusal at all, so the
block recorded in the refusals note was specific to that lane's execution policy.
The push was then rejected by the remote on the merits:

    ! [rejected]  cad0fd0127270b607bb57d5a1bc4dcf9832f248a -> main (non-fast-forward)
    git-push-verified: ... origin refs/heads/main is unchanged at 3dbe3be0ce1d
    (re-read after the failure, not assumed). NOTHING LANDED

**The work already landed by another route while this lane was blocked.**
`origin/main` moved three commits ahead of this commit's parent `a9505da1`, and two
of the three touch exactly the same two files and do the same job:

- `779bee63a` Retain publisher result when retry launch fails
- `5a09833de` Preserve publisher failures through health rendering

Task `validation-ledger-and-series-publication-are-both-stalled` is closed with
`landing=landed reference=301 resolved=rrnewton/dev-hermit@779bee63a`, i.e.
https://github.com/rrnewton/dev-hermit/pull/301 .

**Main is a strict superset, verified rather than eyeballed.** All three directions
this commit widened are closed in main with the same exit statuses — the retained
unit's `Result` and `ExecMainStatus` read on the next sweep, a publisher still active
at the next sweep, and a launcher refusal. Both versions have the same eight exit
points (seven `exit 1`, one `exit 0`) and both carry `--collect` only in the comment
explaining its removal. Cross-running THIS commit's test against main's launcher gives
**43 forward checks passing and zero forward failures**; the 11 failures are all the
pre-fix `baseline` arm, which rebuilds the old launcher by literal text substitution
that no longer matches main's refactored source.

**Two things main has that cad0fd01 does not:**

1. At the final "could not launch" path main prefixes `${PRIOR_FAILURE:+$PRIOR_FAILURE; }`,
   so a prior failure plus a failed retry launch reports BOTH. cad0fd01 reports only
   the launch failure and drops the prior failure there — a discarded-information path,
   which is the direction the original defect ran in.
2. `emit_result` / `emit_logged_result` put `state=` and `summary=` on separate lines so
   tick-hub renders the real diagnostic instead of an unresolved `{summary}` placeholder,
   and collapse embedded newlines. cad0fd01 has zero tick-hub coverage; main's test has
   seven references.

**Landing this commit would require a force-push over three landed commits**, two of
which carry the same fix — deleting the better version to install a weaker copy of it.
Declining to push was correct. Nothing here needs rescuing; abandon the commit.

Known open edge, not a doubt about main: main's own tick-hub assertions were not verified
in a populated checkout. A scratch worktree could not populate the `agent-utils` submodule,
so `agent-utils/py/bin/tick-hub` was missing and 6 of main's own checks failed for that
reason (68 passed). The tool exists in the parent checkout.
