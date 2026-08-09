# Urgent Critical-Path Fix Validation

Use this coordinator protocol only for an owner-declared deadline or the rare
quantified critical-path bottleneck defined by
[parallel-speculative-attack](parallel-speculative-attack.md). Record the gate
and deadline before starting. Routine fixes use the normal single-path workflow.

## Run the outer loop in parallel

For each coherent candidate head, start both tracks immediately:

1. Push the explicit ref and launch GitHub CI for that SHA.
2. In its isolated worktree, run the focused checks and `./scripts/validate.rs` (or every
   locally runnable gate the change can affect).

Do not wait for the slow CI result before beginning local validation. Let local
evidence drive the next fix while CI runs. Batch fixes into a coherent head
before pushing again; do not flood CI with every inner-loop edit.

Local validation does not silently replace the repository merge gate. If the
owner/task explicitly authorizes urgent landing on local evidence, bind the
evidence to the exact SHA, land, keep watching PR/main CI, and fix forward. With
no explicit exception, authoritative CI must still be green before landing. If
`scripts/validate.rs` has a known host-only baseline failure, prove it on clean main and
report the exact focused passes instead of claiming a false green.

## Babysit CI

Assign an active watcher; do not treat CI as a timer. Track queue time, runner
assignment, job/step transitions, last log progress, and relevant host/resource
conditions. Use `with-proxy gh pr checks --watch` and `gh run view --log-failed`
or equivalent run logs. Detect and report a stuck queue, runner, or silent job
quickly, preserving the run URL and last-progress timestamp. Retry only after
distinguishing infrastructure stall from a product test failure.

## Keep the inner loop tight

When CI exposes a blocker:

1. Extract the exact failing test, command, flags, environment, and first useful
   error from the earliest signal.
2. Reproduce that individual test locally and loop it tightly until the failure
   is understood and the fix survives repeated runs.
3. Expand once to its containing suite, then local validation, then push one new
   coherent SHA to the parallel CI track.

**Never use a 40-minute CI workflow as the inner debugging loop.** CI confirms a
candidate; the individual local test discovers and nails the blocker.

For a hard determinism or scheduling blocker, use the workspace
`research-planning-persona` skill at
`~/work/dev-hermit/.llms/skills/research-planning-persona/SKILL.md` before spawning
competing approaches. Otherwise stay frugal: one fix path, one tight test loop,
one watched CI run per coherent head.
