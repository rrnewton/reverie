# GitHub Actions disable directive and Reverie branch-protection conflict

Date of this snapshot: 2026-08-17.

This document records two deliberate policies that currently cannot both be
satisfied. It does not recommend changing either policy and does not authorize a
repository-setting or branch-protection change.

## Owner directive still in force

Task `owner-directive-disable-github-actions-entirely` records this owner
directive verbatim:

> GH CI is SECONDARY and we must use local validation doesn't seem to stick.
> Agents see it and get confused. For now I want you to DISABLE GH actions
> entirely. Just turn it off. We will get back to it when I have time to audit
> it.

The task explicitly named `rrnewton/hermit` and `rrnewton/reverie`, required the
repository-level Actions setting to be disabled, required local validation to
remain available, and said:

> Do NOT delete workflow files. Disabling is reversible; deletion is not, and
> the owner said he will audit it later.

The implementation note at `2026-08-11 17:24` records that both repositories
were changed from `enabled=true allowed_actions=all` to repository-level
`enabled=false`. On 2026-08-17, the live API still returned `enabled=false` for
both repositories. Their workflow files remain present, and the relevant
workflows remain individually `active` underneath the repository-level switch.

The task also records that the self-hosted runners deliberately remained
registered and online. On 2026-08-17, four Hermit and three Reverie self-hosted
runners were still online and idle. Their availability does not override the
repository-level setting: frozen runs contain no job records and therefore
never reach runner assignment.

## Reverie branch protection still requires hosted `merge-gate-v2`

On 2026-08-17, live protection for `rrnewton/reverie:main` had
`required_status_checks.strict=true` and required the status context
`merge-gate-v2`.

The producer has not been renamed or deleted. Reverie's active
`.github/workflows/merge-gate.yml` contains a job named `merge-gate-v2`.
Repository-level `enabled=false` prevents that active workflow from producing
the required check.

Therefore, while both policies remain unchanged, a current Reverie pull-request
head cannot obtain the required hosted check.

## Immediate consequence: Reverie PR #443 and M3 fail-closed

At this snapshot, [Reverie PR #443](https://github.com/rrnewton/reverie/pull/443)
was open and non-draft at exact head
`e7972364634aae3ef62705527c70a1c0556c5784`. GitHub reported it `MERGEABLE` and
`BEHIND`, not conflicting, with zero checks. Exact-head Claude and Codex reviews
approved it with no blocking code finding. GitHub's `reviewDecision` field was
empty because the project's review authority is recorded in exact-head comments,
not native GitHub review state.

PR #443 cannot produce `merge-gate-v2` while repository Actions are disabled.
Hermit's staged M3 fail-closed change depends on the Reverie change in #443, so
this policy conflict also blocks that dependency chain.

## Five-day cost visible on 2026-08-17

The GitHub run timeline brackets the start of frozen Hermit runs on 2026-08-11:

- The last pre-disable Hermit run completed at `2026-08-11T17:11:59Z`.
- The disable task records its implementation at `2026-08-11 17:24`.
- The first later frozen run was Docs run `31519244713`, created at
  `2026-08-11T17:46:02Z`, twenty-two minutes after the recorded setting change.
  It remains `queued` with zero job records.

By the 2026-08-17 snapshot, the conflict had persisted for five days. The live
open-PR population was:

- Hermit: 80 open, including 59 non-draft; all 59 had zero check rollup.
- Reverie: 16 open, including 11 non-draft; 10 of 11 had zero check rollup.
- Combined ready non-draft population: 70 pull requests.

The queue cannot obtain fresh hosted validation while the repository-level
switch remains off. Every commit landed during the overnight effort was also
observed with zero check-runs. Absence of a hosted check is expected under the
disable directive, but it cannot simultaneously satisfy Reverie's strict
required-check policy.

## Owner options, without recommendation

Each option changes a deliberate owner policy. This document records the
choices without selecting one:

1. End the disable directive and re-enable repository Actions. The original
   state recorded by the task was `enabled=true allowed_actions=all`; restoring
   it would allow the existing active workflow to produce `merge-gate-v2`.
2. Revise Reverie branch protection so its required check can be produced while
   repository Actions remain disabled.
3. Narrow the disable directive, for example by keeping Actions disabled on
   Hermit while enabling it on Reverie, if the confusion cited by the directive
   is judged to require different repository scope.

Leaving both present policies unchanged preserves the owner's temporary hosted
CI shutdown and also preserves a required check that no current Reverie PR can
produce.

## Read-only evidence used for this snapshot

The following sources were read without changing settings or branch protection:

```text
tg owner-directive-disable-github-actions-entirely -v
GET repos/rrnewton/hermit/actions/permissions
GET repos/rrnewton/reverie/actions/permissions
GET repos/rrnewton/reverie/branches/main/protection
GET repos/rrnewton/{hermit,reverie}/actions/workflows
GET repos/rrnewton/{hermit,reverie}/actions/runners
gh pr view 443 --repo rrnewton/reverie
gh pr list --repo rrnewton/{hermit,reverie} --state open
gh run view 31519244713 --repo rrnewton/hermit
```

The counts and repository state are dated observations. They must be re-read if
the owner ends or narrows the disable directive, changes Reverie protection,
updates PR #443, or the open-PR population changes.
