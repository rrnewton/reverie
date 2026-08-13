# Do today's landed fixes behave as claimed? Six verdicts

2026-08-12, ~21:10 PDT. Read-only. Method throughout: exercise the running mechanism, not the
diff and not the task note. **Nothing found here was fixed** — a fix discovered to be inert is
a finding.

## Summary

| # | fix | verdict |
| --- | --- | --- |
| 1 | `d2937f83` approval-binding parser | **VERIFIED** — 6 crafted cases, both directions |
| 2 | `cff6f574` closure rule | **SPLIT** — text and gateway VERIFIED; the standing ancestry audit **NOT-VERIFIED** |
| 3 | `b23eb43b` who-am-i resolver | **VERIFIED** — live AGREEMENT, 37/37 reproduces |
| 4 | `78c52589` validate-lock quarantine | **SPLIT** — dead-identity citation VERIFIED; release-on-clean-completion **CANNOT-VERIFY-HERE** |
| 5 | `gate_runner_presence` | **VERIFIED** — emits, and returns NO_RESULT rather than guessing |
| 6 | `b5a4748e` denominator | **VERIFIED FROM RECORD** — nothing dropped; a fresh run is CANNOT-VERIFY-HERE |

All five parent commits and `b5a4748e` were confirmed ancestors of their repo's freshly
fetched `origin/main` before anything else — the "reported as possibly-landed when ancestry
showed it was not landed" case does not recur here.

## 1. `d2937f83` — VERIFIED

First closed the trap the fix is famous for. The tree can serve old code while main carries
the fix, so:

```
diff <(git show origin/main:ci-hub/health/approval_binding.py) ci-hub/health/approval_binding.py
  -> IDENTICAL
__pycache__/approval_binding.cpython-312.pyc  2026-08-12 19:25   (same minute as the .py)
```

Then executed that parser on crafted fixtures through its own `--from-json` interface,
bracketing both directions as policy requires:

| case | input | expected | got |
| --- | --- | --- | --- |
| control | `APPROVED-AT: claude <head>` | BOUND_CURRENT | BOUND_CURRENT |
| **the fix** | approval + `## CHANGES-REQUESTED-AT: claude <head>` | UNDETERMINED | UNDETERMINED |
| the other direction | `## APPROVED-AT: claude <head>` alone | UNDETERMINED | UNDETERMINED |
| `>` prefix | approval + `> CHANGES-REQUESTED-AT:` | UNDETERMINED | UNDETERMINED |
| `-` prefix | approval + `- CHANGES-REQUESTED-AT:` | UNDETERMINED | UNDETERMINED |
| re-approval | approval, `##` rejection, approval | BOUND_CURRENT | BOUND_CURRENT |

6/6. The prefixed approval is additionally *reported* as a malformed source line rather than
silently dropped.

## 2. `cff6f574` — text and gateway VERIFIED, standing audit NOT-VERIFIED

**Text (verified).** `AGENTS.md` in this checkout is byte-identical to `origin/main`,
`CLAUDE.md` is a symlink to it, and rule 15 reads *"Close only after landing"* with the
`close-task` gateway named. (I can speak only for this checkout; another checkout serving
something else is a separate question.)

**Gateway (verified by use).** I exercised `./ci-hub/bin/close-task` three times today. It
`VERIFIED` and `CLOSED` when the reference was a real ancestor, and it **refused** —
`UNVERIFIABLE … rc=2` — when pointed at a source checkout whose origin did not match the named
repository. It is bound to the fact it claims.

**The standing audit does not agree with the rule it is meant to enforce.**
`ci-hub/closure/ancestry_audit.py`, described in its own header as *"Standing post-landing
ancestry audit: which implemented tasks are ACTUALLY on main?"*, reports:

```
LANDED 58   NOT-LANDED 0   UNKNOWN 0
closable on ancestry now : 58
real drain queue         : 0
```

while 17 non-draft PRs are open and none has a qualifying receipt. It reaches LANDED by
testing whether **any** SHA or PR associated with a task is on main, rather than the change's
own commit. Its own emitted record says so:

```json
{"task": "p1_compat_lsof_is", "bucket": "LANDED",
 "sha": "1f4e430c135204591daa0c0cba84f77dae679a78",
 "why": "note SHA in hermit",
 "pr_states": ["hermit#2221:OPEN", "hermit#2176:OPEN(bare)"]}
```

`1f4e430c…` **is hermit main's own tip** — trivially an ancestor of main — and it was merely
quoted in a task note. The contradicting evidence (`pr_states: OPEN`) is in the same object and
is not consulted.

Three distinct mis-binding modes across six of the nineteen implemented-and-open tasks:

| mode | `why` | example | reality |
| --- | --- | --- | --- |
| a note-quoted SHA that is main itself | `note SHA in hermit` | `p0_kvm_cannot_reach`, `p0_privileged_e2e_manifest`, `p1_compat_lsof_is` | PRs #2217, #2215, #2221 all OPEN |
| the merge commit of a *different*, incidentally-mentioned PR | `mergeCommit of bare PR #2071->hermit` | `compat_bin_c_robust`, `p1_promoting_an_e2e` | their own PRs #2223, #2220 OPEN |
| a cross-repo SHA | `note SHA in dev-hermit` | `port-peer-snapshot-protocol-into-validate-rs` | its hermit PR #2222 OPEN |

**Why this is the dangerous one.** Acting on "closable on ancestry now: 58" would close tasks
whose work is not landed — exactly the phantom closure the corrected rule exists to prevent,
produced by the tool that is supposed to prevent it. Not fixed, per instruction.

## 3. `b23eb43b` — VERIFIED

Live invocation:

```
state=AGREEMENT
agent=hermit-bpf-research
basis=caller-environ,process-ancestry
signal caller-environ    resolved
signal process-ancestry  resolved
signal process-model     unavailable
signal cwd-slot-owner    unavailable
signal pane-owner        unavailable   detail=pane=%37
signal cgroup-owner      unavailable
```

Two independent signals agree; four are unavailable and **say so** rather than being guessed.
`pane-owner` being unavailable is the point of the item: resolution no longer depends on the
pane mapping. The suite reproduces the claimed number exactly — `37 passed in 0.71s`.
`model=unresolved` with an empty `model_source` is a separate, disclosed gap, not an identity
failure.

## 4. `78c52589` — split

**"Stops citing payload identities that `/proc` shows are dead" — VERIFIED.**
`ci-hub/lib/landing_lock.rs:2396` evaluates `exact_process_liveness(identity)` **per call** and
pushes to `active` only on `Ok(true)`. Live `validate-lock status` now reports *"all recorded
residual identities and the captured process group are absent"* and refuses through the
documented incomplete-domain-capture branch, not through a stale "active" claim. I cannot
reconstruct whether those PIDs were alive at 02:39:59Z when hermit-908's log did cite them, and
I make no claim either way; the observable behaviour now is correct.

**"Releases on clean completion" — CANNOT-VERIFY-HERE.** That needs a validate run to complete
cleanly on this box, which it cannot: the quarantine blocks admission and the CPUID gap fails
the privileged cell. No run since the fix landed has completed cleanly here.

## 5. `gate_runner_presence` — VERIFIED

```
GATE RUNNER NO_RESULT -- hermit-gate-runner.service @ .../ci-runner/state-gate
  ABSENT     installation
  ABSENT     credentials
  ABSENT     service
  NO_RESULT  registration   no local identity to look up (credentials surface failed first)
  admission must treat this as NO_RESULT
```

It emits, it separates four independent surfaces, and where a surface cannot be evaluated it
returns `NO_RESULT` with the reason instead of inferring an answer from the others. That is the
claimed behaviour, demonstrated against a genuinely absent surface.

## 6. `b5a4748e` — verified from record, not re-run

| receipt | executed tests | coverage | checks |
| --- | --- | --- | --- |
| `b5a4748e` 2026-08-10T01:30:08Z | **862** | **19/19** | 55 |
| latest greens 2026-08-12 | **1049–1060** | **19/19** | 58–59 |

Nothing silently dropped: the coverage denominator is intact and both the test count and the
check count rose. Deliberately filtered to `exit_code == 0` full-profile rows — the same shard
holds 39 rows at `0/19` and 11 at `4/19`, and folding truncated runs into this trend would be
the exact denominator error the item asks about. A fresh run on this box is
CANNOT-VERIFY-HERE.

## Correction to the brief

The task credits me with refuting a `kthreadd` claim by measuring inside the bwrap namespace. I
did not do that and have no record of it; it was another agent's measurement. Correcting it
here because a misattributed verification is the same class of unbound claim this task exists
to catch.

## Commands

```sh
git merge-base --is-ancestor <sha> origin/main            # per commit, freshly fetched
diff <(git show origin/main:<path>) <path>                # tree-serves-landed-code check
python3 ci-hub/health/approval_binding.py --from-json /tmp/fx.json --json
python3 ci-hub/closure/ancestry_audit.py --json /tmp/aa.json
./ci-hub/bin/who-am-i ; python3 -m pytest ci-hub/lib/tests/test_agent_identity.py -q
python3 ci-hub/health/gate_runner_presence.py
./ci-hub/ci-hub validate-lock status
# ledger/hermit/devbig014/2026-08.jsonl -> legacy_row.{executed_tests,coverage,checks}
```
