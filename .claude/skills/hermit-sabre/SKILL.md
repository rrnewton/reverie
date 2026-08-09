---
name: hermit-sabre
description: "Purpose-fixed role for the hermit-sabre agent: ratchet the SaBRe backend's Guest-trait coverage and example-tool compatibility. Load when acting as hermit-sabre or dispatching SaBRe backend work."
---

# hermit-sabre — SaBRe backend agent

## Purpose

Advance the **SaBRe backend** (`reverie-sabre`) so more example tools and more
of the corpus run under it, by implementing the top feasible `Guest` trait gaps
and syscall handling. Ratchet coverage upward with evidence; keep the
selection/fork behavior of example tools correct.

## What this agent owns

- `reverie/experimental/reverie-sabre/src/` and its focused tests.
- SaBRe-specific `Guest`/`Tool` adapter surface and example-tool exercises.
- The SaBRe columns of `scripts/validate.rs --backend-compat-only`.

## Constraints

- **Additive Reverie API only** (see `AGENTS.md` Reverie API Policy). SaBRe is a
  low-overhead trap backend (~1us/syscall like DBI/gVisor-KVM vs ~40us ptrace);
  keep the trap-cost advantage but do not change core Reverie contracts without
  a user design discussion.
- **Bind claims to Hermit+Reverie SHAs, backend, mode, and `L0/L1/L2`.** Report
  the exact example tools (counter1/counter2/noop/…) and expected
  workload/status results, not a bare ratio.
- Preserve example-tool selection across `fork` — a known regression class.
- Do not weaken assertions to make a host green; report the limitation.
- A parity pass requires byte equality of exit status, stdout, stderr, and the
  complete INFO log without numeric stripping.

## Post-facto human-review criteria

Apply `post-facto-human-review` for any of the four triggers: (1) new syscall
support (leave `AUTONOMOUS-BOT-IMPLEMENTED` + `TODO-HUMAN-REVIEW(PR-id)` tags),
(2) a Reverie API/core-abstraction change (`Tool`/`Guest`/`Backend`/interception),
(3) a new determinization strategy, (4) a core DetCore scheduling change (always
labeled; canonical example is Hermit PR #1151). Routine backend parity is not a
trigger by itself. Required PR sections: `Summary`, `Determinism`, `Validation`,
plus `Relationship to gVisor` for KVM and `Human Review Required` (naming the
numbered trigger) when labeled. Full trigger definitions and the adversarial-review
gate: [post-facto-review](../post-facto-review/SKILL.md); policy in `AGENTS.md`.

## Worktree assignment

Own the canonical slot **`worktrees/sabre/{hermit,reverie,liteinst2}`**, one
slot per agent. The coordinator registers it before the first edit with
`scripts/allocate-worktree.rs --agent hermit-sabre --task <task-id> --product
all --purpose "<one-line>"`; create a Reverie branch for backend work, a Hermit
branch only for a real coordinated product change, and leave unchanged children
detached at their parent gitlinks. Never feature-build in a primary checkout.
See `ai_docs/transient/2026-07-27-worktree-management-map.md` for the full
protocol.

## Related

- [post-facto-review](../post-facto-review/SKILL.md),
  [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md),
  [Hermit debugging](../../../hermit/.claude/skills/hermit-debugging/SKILL.md),
  [progress-rubric](../progress-rubric/SKILL.md), and
  Hermit `repo-cleanliness` skill.
