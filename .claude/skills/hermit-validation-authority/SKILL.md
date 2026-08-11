---
name: hermit-validation-authority
description: "Hermit's validation authority and single source of truth for evidence, rebase-inherited landability, red-main repair, and local-versus-GitHub selection. Use for every Hermit validation, landing, or evidence decision."
---

WE ARE IN LOCAL-VALIDATE MODE ONLY.

# Hermit validation authority and evidence

This file is the only policy source for three questions:

1. Which Hermit test system decides whether a change is validated?
2. What evidence does that system have to record?
3. Does local validation or GitHub-hosted testing govern landing today?

Other files may document commands, storage, or implementation. They must link
here instead of defining different answers.

## Red `main` is the highest-priority incident

When VALIDATE runs on the fetched `rrnewton/hermit:main` commit and
`ci-hub validate-status --sha <main-commit>` reports a recorded failure,
repairing that failure is
[priority P0](../../../AGENTS.md#red-tip-direct-land-owner-authorized-exception-to-hard-invariant-9).
Stop landing unrelated pull requests,
make only the repair, run VALIDATE on the repair commit, and use the
[Red-Tip Direct Land procedure](../../../AGENTS.md#red-tip-direct-land-owner-authorized-exception-to-hard-invariant-9).
Do not wait for GitHub-hosted testing. The owner's rule is: speed outranks that
ceremony because `main` is already red, and local validation is sufficient.

The exception ends as soon as `ci-hub validate-status --sha <repair-commit>`
accepts the repair commit's LEDGER row. A red GitHub-hosted job does not by
itself establish that `main` is red under this rule.

## The authority, record, and label

- [VALIDATE](../../../hermit/scripts/validate.rs) is the full Hermit validation
  program. A qualifying run is started only by
  [`ci-hub validate-run`](../../../ci-hub/validate/start_unit.py).
- The [LEDGER](../../../ci-hub/ledger/README.md) is VALIDATE's durable record.
  A completed run first appears as an immutable event under
  `ignored/ci-hub/validate-ledger-spool/`. Publication copies that event into
  the tracked append-only shard
  `ledger/<team>/<short-machine>/<UTC-year>-<UTC-month>.jsonl`.
  [`validate_rows.py`](../../../ci-hub/ledger/validate_rows.py) unions the live
  and tracked copies so every reader sees one logical record.
- [`ci-hub validate-status`](../../../ci-hub/lib/validate_status.rs) is the one
  reader that decides whether a LEDGER row qualifies. A shell exit code, log
  summary, GitHub label, comment, or copied JSON field cannot replace that
  decision.
- The [`locally-validated` label](../../../ci-hub/ci-hub.rs) on a Hermit GitHub
  pull request is derived from the qualifying LEDGER row by
  `ci-hub apply-local-label`. The label is a convenience for people scanning a
  pull request, not evidence of its own. Never type or apply it by hand.

## One production path

Run every LEDGER-writing VALIDATE through this command from the dev-hermit root:

```sh
./ci-hub/ci-hub validate-run --checkout <registered-hermit-worktree> \
  --agent <agent-name> --target <40-hex-hermit-commit> --pr <pull-request> -- full
./ci-hub/ci-hub validate-status --sha <40-hex-hermit-commit>
```

The checkout argument names a
[registered Hermit worktree](../../../AGENTS.md#strict-slot-pool).
[`ci-hub validate-run`](../../../ci-hub/validate/start_unit.py) performs
admission, obtains the
[machine validation lock](../../../ci-hub/lib/validate_lock.rs), starts a
detached user service, invokes
[VALIDATE](../../../hermit/scripts/validate.rs), retains the log, and records
the result in the LEDGER. Do not invoke `scripts/validate.rs` directly for a
qualifying run; that would create a second production path.

If the calling agent exits, attach to the service handle printed by
`ci-hub validate-run`. Do not launch a replacement run merely because the
original caller disappeared.

## Rebase before validation

**ALWAYS REBASE BEFORE VALIDATE.**

Before admitting VALIDATE, `ci-hub validate-run` must compare the candidate
commit with freshly fetched `origin/main`. If the candidate lacks any commit
already on `origin/main`, admission must return an error before it acquires the
validation lock or writes a LEDGER event. Rebase first, then submit the new
commit to `ci-hub validate-run`.

This is a mechanical refusal, not advice. An escape flag, a missing remote ref,
or an unfetched local `origin/main` must not turn an out-of-date base into a
qualifying validation run.

## What a qualifying LEDGER row says

The current field contract is implemented by
[`validate_status.rs`](../../../ci-hub/lib/validate_status.rs) and stored in the
[append-only LEDGER](../../../ci-hub/ledger/README.md). A qualifying row must
identify all of the following:

- the exact 40-hex Hermit commit and its Git tree;
- a clean worktree whose files equal that commit;
- the `full` VALIDATE profile and the set of
  [validation nodes](../../../hermit/ci/dag/) that profile was required to run;
- `executed_tests > 0`, where `executed_tests` counts Rust tests reported by
  the test processes that [VALIDATE](../../../hermit/scripts/validate.rs)
  launched;
- `filtered_tests`, where `filtered_tests` counts tests those test processes
  reported as filtered out; zero filtered tests does not prove that every
  required validation node ran;
- zero failed validation nodes; a Rust test failure makes the node that ran it
  fail;
- the required validation nodes, and a record that every required node
  completed; and
- the short machine name that ran it and the retained VALIDATE log at
  `ignored/validate/<validation-service-name>.log`.

A killed process, incomplete required-node set, dirty worktree, absent or zero
`executed_tests`, failed node, failed test, missing retained log, or unreadable
LEDGER event does not qualify. The
[versioned ledger format](../../../ai_docs/validate-ledger-team-machine-schema_20260807.md)
documents where these values live; this skill defines what they mean for
landing.

Every validation check needs both test directions: a real qualifying LEDGER row
must be accepted, and a well-formed test row with one required fact missing or
changed must be refused. The test row expected to be refused must not add a
real label, merge a pull request, or otherwise authorize the action it tests.

## Hard green and soft green

**Hard green:** VALIDATE ran on commit X, and `ci-hub validate-status --sha X`
accepted the LEDGER row for X.

**Soft green:** in the owner's words, **if you JUST rebase and don't fix a bunch
of merge conflicts, the new head should SOFT-INHERIT THE VALIDATED STATUS OF THE
OLD HASH.** If hard-green X is rebased onto a newer `main` commit to produce Y,
and zero files require conflict resolution, Y is soft green. If any files
require conflict resolution, the resolver must name those files and record
whether the resolutions are minimal and low risk; a low-risk result is soft
green, while an uncertain or behavior-changing resolution requires VALIDATE on
Y. “Minimal” is a recorded engineering judgment, not an unstated file-count
threshold.

**SOFT GREEN IS LANDABLE.** Record X, the newer base commit, Y, the files that
conflicted, and the resolver's low-risk or revalidate decision in
`ignored/rebase-records.jsonl` through the
[ci-hub rebase recorder](../../../ci-hub/landing/rebase_wrapper.py). A material
code edit, a conflict set the resolver does not judge minimal and low risk, an
unresolved conflict, or a resolver who cannot justify low risk requires a new
VALIDATE run on Y.

Do not copy X's `locally-validated` label to Y and do not write a fake LEDGER
row claiming VALIDATE ran on Y. Landability comes from two truthful records:
the hard-green LEDGER row for X and the recorded X-to-Y rebase.

## How this fits Proxy Binding

[Proxy Binding](../../../ai_docs/policy/proxy-binding-authority-registry.md)
asks: what observable evidence binds a claim to the fact it describes?

- The exact commit in the LEDGER row answers **WHAT DOES THIS EVIDENCE PROVE?**
  A row for X proves that VALIDATE tested X, not Y.
- The recorded X-to-Y rebase answers **IS THE RESULT GOOD ENOUGH TO LAND?** A
  low-risk rebase lets Y soft-inherit X's validated status without pretending
  that VALIDATE tested Y.

Those questions are compatible. Exact-commit evidence does not require the
fleet to discard everything it learned whenever `main` advances. The general
Proxy Binding rule remains in `AGENTS.md`; the validation fields and landing
decision live here so a future evidence-policy change edits one file.

Review, task authorization, conflict-resolution judgment, one-at-a-time
publication through the repository's landing tool, and proof that the landed
commit reached `main` are separate decisions. Hard or soft validation green
does not create any of them.

## GitHub Actions history — off now

[Hermit's GitHub Actions workflows](../../../hermit/.github/workflows/) still
exist and were active through 2026-08-11. They previously supplied hosted test
results and special handling for changes to the validation program.

**GitHub Actions is OFF NOW by owner directive pending the owner's audit.** A
hosted workflow result is not current validation authority, a veto, a second
green, or a landing prerequisite. Do not wait for it, dispatch it, or cite it
as the operative validation decision. For a change that cannot safely validate
the validation program itself, ask the owner to name a one-time validation
procedure; do not silently reactivate the old hosted path.

The owner may reactivate the hosted infrastructure later. Until an owner
directive updates this file, hosted behavior belongs to history and must not be
mixed into the operative rules above.
