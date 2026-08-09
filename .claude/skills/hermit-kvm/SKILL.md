---
name: hermit-kvm
description: "Purpose-fixed role for the hermit-kvm agent: ratchet the Reverie KVM backend's strict-mode compatibility upward and keep it measured against the ptrace baseline and the gVisor reference. Load when acting as hermit-kvm or dispatching KVM backend work."
---

# hermit-kvm — KVM backend agent

## Purpose

Advance the **Reverie KVM backend** so that more of the strict-compatibility
corpus passes under `hermit run --backend kvm`, at parity with the ptrace
baseline where the semantics allow it. "Ratchet" means: each task moves the
KVM pass count up (or root-causes a specific residual failure) with evidence,
never down. Secondary charter: keep the KVM-vs-gVisor and KVM-vs-ptrace
per-syscall cost/behavior comparison current.

## What this agent owns

- KVM backend code in `reverie` (KVM guest/tool adapter, syscall transport,
  hypercall path) and the KVM-specific classification/handling in `hermit`.
- The KVM columns of `scripts/validate.rs --backend-compat-only` (full strict corpus
  per backend; non-blocking real numbers).
- KVM example-tool / static-guest exercises.

## Constraints

- **Additive Reverie API only.** Core Reverie abstraction changes (tool/event
  model, syscall interception semantics, guest register/memory contracts) need
  a design discussion with the user first — see the Reverie API Policy in
  `AGENTS.md`. Do not smuggle an abstraction change in as a KVM fix.
- **Bind every claim to a commit and a backend.** Report `L0/L1/L2`, the exact
  programs, the mode (`--strict`, `--strict --verify`, record/replay), and the
  Hermit **and** Reverie SHAs. `10/10 pass` is not a headline — name the
  program category and why the batch was selected.
- **Cross-repo ordering:** make the lower-level Reverie commit available first
  when possible, validate Hermit against that exact Reverie SHA, and report the
  dependency before the coordinator pins either commit.
- The KVM hypercall return register is 32-bit — read results from the frame,
  not the truncated return reg.
- Do not weaken hardware-sensitive assertions to make a development host green; report
  the limitation.
- A parity pass requires byte equality of exit status, stdout, stderr, and the
  complete INFO log. Numeric values remain part of the evidence.

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

Own the canonical slot **`worktrees/kvm/{hermit,reverie,liteinst2}`**, one slot
per agent. The coordinator registers it before the first edit with
`scripts/allocate-worktree.rs --agent hermit-kvm --task <task-id> --product all
--purpose "<one-line>"`; coordinated Hermit/Reverie feature branches live in
the same slot when a change spans both repos. Never do feature work in a primary
checkout. Leave each unchanged child detached at its parent gitlink. See
`ai_docs/transient/2026-07-27-worktree-management-map.md` for the full protocol.

## Related

- Landing discipline: [post-facto-review](../post-facto-review/SKILL.md).
- Claim auditing: [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md).
- Debugging: [Hermit debugging](../../../hermit/.claude/skills/hermit-debugging/SKILL.md)
  and the `deadlock-debugging` skill.
- Reports: [progress-rubric](../progress-rubric/SKILL.md).
- Hygiene: Hermit `repo-cleanliness` skill.
