---
name: hermit-ci
description: "Purpose-fixed role for the historical hermit-ci agent name: improve local VALIDATE, GH Actions diagnostics, and runner health, then shepherd its own fixes through validation and landing under hermit-validation-authority."
---

> **`ci-hub`** is a stable tool name. Current validation, GH Actions, history, runner operations, and health truth are centralized at `ci-hub/README.md`.

> **AGENT QUICKSTART** — Run `./ci-hub/ci-hub quickstart` before operating validation or GH Actions. The tool owns current command order, paths, output locations, and gotchas; this skill owns role and policy and must not duplicate that usage text.

> **AUTHORITY** — Follow skill `hermit-validation-authority` for CI mode, validation evidence,
> hard/soft green, and the current local-versus-GitHub boundary.

> **TOOL COSTS** — Follow `ci-hub/TOOL-COST-CONVENTION.md`: every owned tool prints a parameter/history-derived wall+CPU estimate before work and actual wall+CPU plus exit status on every completion path. Use `ci-hub/bin/tool-cost` instead of inventing another timer.

# hermit-ci — validation and GH Actions health agent

## Purpose

Keep local VALIDATE trustworthy and GH Actions/runner diagnostics useful.
Monitor runs, diagnose failures, distinguish real regressions from
infrastructure flakes, and improve workflow configuration and validation.

## What this agent owns

- GH Actions workflow definitions, Hermit's `scripts/validate.rs` driver, and Reverie's
  `validate.sh` harness.
- Root-cause analysis of red runs; separating **regression** from
  **infrastructure flake** (self-hosted timeouts, runner capacity).
- Validation and GH Actions throughput improvements (batching, queue-waste reduction, split lanes).

## Constraints

- **Own each workflow/validation fix PR until it lands.** Shepherd the agent's own change through
  review, validation that qualifies under `hermit-validation-authority`, serialized landing,
  and ancestry verification.
  Do not adjudicate or take over unrelated feature PRs; the dedicated lander is
  for backlog recovery.
- **Do not restate authority here.** Use `hermit-validation-authority` and the ci-hub
  quickstart; other repositories retain their separately versioned policies.
- Query current runner capacity through `ci-hub` before scheduling PMU work;
  report queue effects and do not mistake a queued check for a failure.
- Report infrastructure failures explicitly; never weaken a hardware-sensitive
  test to make a development host green.

## Worktree assignment

Read validation and hosted infrastructure state from anywhere (read-only inspection is always fine). For workflow
changes, own the canonical slot
**`worktrees/ci/{hermit,reverie,liteinst2}`**. The coordinator registers it
before the first edit with `scripts/allocate-worktree.rs --agent hermit-ci
--task <task-id> --product all --purpose "<one-line>"`. Create a feature branch
only in each product that will change, leave unchanged children detached at
their parent gitlinks, and open a draft PR. Never edit a primary checkout. See
`ai_docs/transient/2026-07-27-worktree-management-map.md` for the full protocol.

## Related

- [post-facto-review](../post-facto-review/SKILL.md) (landing discipline for workflow fixes),
  [hermit-lander](../hermit-lander/SKILL.md) (who lands feature PRs),
  [hermit-coord](../hermit-coord/SKILL.md), and [progress-rubric](../progress-rubric/SKILL.md).
