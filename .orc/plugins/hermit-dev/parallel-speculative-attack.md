# Parallel Speculative Attack

Use this coordinator protocol only when wall-clock time to a correct decision is
more valuable than the cost of running several deliberately competing attempts.
The common case is one owner, one approach, and one PR.

## Invocation gate

Launch a parallel speculative attack only when one of these gates is documented
in the task before dispatch:

1. **Owner deadline:** the owner explicitly requests deadline-driven parallelism
   and names the deadline or required decision window.
2. **Quantified critical-path bottleneck:** the coordinator shows that this work
   blocks a named goal or release, currently has one active implementation
   attack, records elapsed time against the expected window, and shows stalled
   progress with an objective metric across status checkpoints. Useful metrics
   include an unchanged failing-test count, no smaller reproducer, no narrowed
   first divergence, no viable patch, or no ETA.

The owner-deadline gate is the normal reason to invoke this protocol. The
coordinator-initiated critical-path gate is rare and requires all of its stated
evidence.

Coordinator impatience, spare agents, a slow CI run, or a merely difficult task
does not satisfy the gate. Do not fan out when one approach is already making
smooth, measurable progress. Frugal single-path execution is the default.

Record the gate evidence, agent budget, timebox, and decision owner in a task
note. If the evidence cannot be stated quantitatively, keep one implementation
path and improve its diagnosis instead.

## Generate distinct attacks first

Before assigning implementation agents, load the workspace
`research-planning-persona` skill at
`~/work/dev-hermit/.llms/skills/research-planning-persona/SKILL.md` and run its
evidence-first `Classify -> Localize -> Generate -> Score -> Emit` pipeline. For
non-determinism work, retain the same structure while substituting the relevant
architecture and evidence.

Emit **3-4 genuinely distinct candidate briefs**. Each brief must name:

- a stable descriptive strategy slug and a different mechanism or lever;
- the evidence and code boundary it attacks;
- its correctness or determinism argument;
- the smallest prototype and exact validation command;
- a cheap kill criterion and a fixed timebox;
- expected blast radius and review triggers.

Reject a candidate set that restates the same patch several ways. If planning
produces only one defensible approach, run it as a normal single-path task.

## Launch isolated competitors

For each selected strategy:

1. Allocate a separate canonical slot and feature branch. Never share a writable
   branch, checkout, or build directory. Record every agent, task, branch, and
   owned path in the worktree registry before editing.
2. Tell the agent explicitly: **SPECULATIVE competing draft; do not land.** Give
   it the same problem statement and evidence, but only its own candidate brief,
   validation target, timebox, and kill criterion.
3. Require an early draft PR once the approach is coherent. The author writes
   the five-field disclosure required by `AGENTS.md` at the start of the PR
   description, names the strategy slug, links the umbrella task, and reports
   exact SHAs and validation without claiming selection.
4. Apply the `speculative` label immediately:

   ```bash
   with-proxy gh pr edit <PR> -R <repo> --add-label speculative
   ```

Do not let speculative fan-out consume the whole fleet. Cap it at the planned
3-4 competitors, preserve capacity for the rest of the critical path, and stop
an attempt as soon as its kill criterion fires.

## Link the competing PRs

As soon as two PRs exist, add a coordinator comment to every competing PR. Keep
the body unprefixed and post it from the `dev-hermit` parent through the
registered boundary:

```bash
with-proxy ./ci-hub/bin/gh --who AGENT --team TEAM --role coordinator \
  pr comment PR --repo OWNER/REPO --body-file BODY
```

The three boundary flags precede `pr comment`; the boundary resolves the model
and prepends the disclosure. The body links the complete set, strategy slugs,
current head SHAs, timeboxes, and the selection rubric. Update the same comment
or add a new attributed comment when the set changes. No PR should look like an
independent landing candidate.

Evaluate all surviving approaches against the rubric declared before launch:
correctness and determinism first, then focused validation, blast radius,
maintainability, performance, and time-to-green. Compare evidence at exact head
SHAs. The first green result is evidence, not automatic victory.

## Select one and close the rest

The coordinator owns the decision:

1. Choose the best supported approach, record why it won, and post the decision
   with `[coordinator, MODEL]` on every competing PR.
2. Promote only the winner into the normal review and CI workflow. Remove the
   `speculative` label from the winner after documenting the promotion.
3. Close every losing PR promptly with the winner link and concise rejection
   evidence. Do not merge alternatives together opportunistically; any borrowed
   idea becomes a new winner revision and must be revalidated and rereviewed.
4. If no approach meets the bar, close all competitors, preserve their evidence
   in task notes, and return to research or a frugal single path.

Retain losing branches until their commits are reachable or the coordinator has
archived the exact SHAs. Release slots only through the registry-aware lifecycle.
Close the umbrella task once the winner's PR is published and its evidence is
recorded; tag it `implemented` so the landing stays tracked, and do not hold the
task open waiting for the merge.

## Exit conditions

The speculative attack ends when a winner is promoted, all candidates are
killed, the owner deadline expires, or the quantified bottleneck clears. At
exit, record the gate, candidate/PR matrix, winner or no-winner decision, exact
SHAs, validation, closed PRs, and released or retained slots.
