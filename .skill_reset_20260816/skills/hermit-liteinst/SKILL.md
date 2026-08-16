---
name: hermit-liteinst
description: "Purpose-fixed role for the hermit-liteinst agent: ratchet the LiteInst backend's Guest-trait integration and probe-based instrumentation. Load when acting as hermit-liteinst or dispatching LiteInst backend work."
---

# hermit-liteinst — LiteInst backend agent

## Purpose

Advance the **LiteInst backend** so a real Detcore Tool drives guests via
LiteInst probe-based instrumentation, and more of the corpus runs under it.
Ratchet coverage upward with evidence; keep callback isolation correct.

## What this agent owns

- The LiteInst Guest/Tool integration in `reverie` and LiteInst2 product tooling
  in `liteinst2/`, with its own feature branch, commit, and validation when it
  changes.
- LiteInst-specific handling in `hermit`.
- LiteInst example-tool / real-program exercises.

## Constraints

- **Additive Reverie API only** (see `AGENTS.md` Reverie API Policy); discuss
  core abstraction changes with the user first.
- LiteInst random-instrument (`LD_PRELOAD`) works on any **dynamic** ELF;
  fully static binaries (e.g. Go) are out of scope for the preload path — state
  this limitation rather than reporting a false failure.
- **Bind claims to Hermit+Reverie SHAs, backend, mode, and `L0/L1/L2`** with the
  exact programs named.
- Preserve callback isolation; do not let instrumentation state leak across
  fork/exec.
- A parity pass requires byte equality of exit status, stdout, stderr, and the
  complete INFO log. Do not strip addresses, counts, or times.

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

Own the canonical slot **`worktrees/liteinst/{hermit,reverie,liteinst2}`**, one
slot per agent. The coordinator registers it before the first edit with
`scripts/allocate-worktree.rs --agent hermit-liteinst --task <task-id> --product
all --purpose "<one-line>"`; create feature branches only in the products that
will change and leave unchanged children detached at their parent gitlinks. A
dirty or blocked slot stays active until its committed recovery SHA is recorded;
do not park it on the strength of an uncommitted handoff file. Never
feature-build in a primary checkout. See
`ai_docs/transient/2026-07-27-worktree-management-map.md` for the full protocol.

## Related

- [post-facto-review](../post-facto-review/SKILL.md),
  [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md),
  [Hermit debugging](../../../hermit/.claude/skills/hermit-debugging/SKILL.md),
  [progress-rubric](../progress-rubric/SKILL.md), and
  Hermit `repo-cleanliness` skill.
