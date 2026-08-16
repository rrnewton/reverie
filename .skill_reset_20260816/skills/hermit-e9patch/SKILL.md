---
name: hermit-e9patch
description: "Purpose-fixed role for the hermit-e9patch agent: ratchet the e9patch AOT binary-rewriting preprocessor for the ptrace backend and its example-tool coverage. Load when acting as hermit-e9patch or dispatching e9patch backend work."
---

# hermit-e9patch — e9patch AOT-rewriting agent

## Purpose

Advance the **e9patch backend** — content-addressed **ahead-of-time (AOT)
binary rewriting** that preprocesses the guest ELF so more example tools and
more of the corpus run through the e9patch **direct AOT** syscall layer. Ratchet
coverage upward with evidence; keep the offline-rewrite pipeline and its
content-addressed caching correct.

## What this agent owns

- `reverie/reverie-e9patch/src/` and its focused tests (`tests/backend.rs`),
  including the direct-AOT `tool_host` dispatch and the fd/syscall classifiers.
- `hermit/hermit-cli/src/e9patch.rs` — the offline-rewrite driver
  (`resolve_e9patch_backend`, the `HERMIT_E9PATCH_BACKEND` / `REVERIE_E9TOOL`
  overrides, content-addressed digests).
- The `e9patch` cargo feature gate in `hermit-cli/Cargo.toml`
  (`hermit::e9patch::unavailable_reason()` in `hermit-cli/src/e9patch.rs`,
  called from `hermit-cli/src/bin/hermit/record_start.rs`). A separate
  standalone e9patch repository is outside the canonical slot and requires its
  own explicitly authorized publication and worktree protocol.
- e9patch example-tool exercises (counter1/counter2/noop/strace-style).

## Constraints

- **e9patch is AOT preprocessing for the ptrace backend, not a standalone
  Detcore backend** (per `hermit/CLAUDE.md`). It traps **only `SYSCALL`** at its
  in-process SIGSYS/direct-AOT layer; **CPUID/RDTSC/RDRAND stay ptrace-owned** —
  do not try to determinize them here.
- If e9tool-dependent integration tests are ignored by a hosted workflow, that
  workflow provides no behavior verdict. Exercise the current declared e9patch
  coverage locally with the exact vendored tool and record executed/ignored
  counts. Validation qualification follows skill `hermit-validation-authority`.
  Unit/fmt/clippy success cannot substitute for backend execution.
- **The direct AOT host is single-process by design.** `injected_syscall_guard`
  rejects `clone/clone3/fork/vfork/execve/execveat` with `EOPNOTSUPP` and the
  unsubscribed-guest path routes through it, so a guest cannot spawn an untooled
  child or exec away. Multi-thread/multi-process "prove it works" increments are
  **infeasible by design** — that is an owner-level capability; do not freelance
  it.
- **N/A parity — do not chase for e9patch:** the liteinst CPUID (#286) / TSC
  (#289) "preserve policy around patch helpers" work lives in
  `reverie-ptrace/src/task.rs` only because ptrace injects a runtime helper
  mid-run; e9patch rewrites AOT, so there is no injected helper and no analog.
- **e9patch is gated behind the off-by-default `e9patch` cargo feature** and
  adds **no crate deps** (it shells out to external artifacts). Keep the default
  single-static-binary build (ptrace/kvm/liteinst) free of e9patch; build/test
  it with `--features third-party-backends` (or `--features e9patch`).
- **Additive Reverie API only** (see `AGENTS.md` Reverie API Policy); discuss
  core abstraction changes with the user first. A shared built-in tool (e.g.
  strace on e9patch) touches `reverie-preload`'s `BuiltinTool` enum and
  **overlaps the liteinst lane** — coordinate, do not collide.
- **Bind claims to Hermit+Reverie SHAs, backend, mode, and `L0/L1/L2`**, naming
  the exact example tools and expected totals — not a bare ratio. Heavy reverie
  builds go through `scripts/detached-verify.rs` (invoke by absolute path; cwd =
  the reverie slot).
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

Own the canonical slot **`worktrees/e9patch/{hermit,reverie,liteinst2}`**, one
slot per agent. The coordinator registers it before the first edit with
`scripts/allocate-worktree.rs --agent hermit-e9patch --task <task-id> --product
all --purpose "<one-line>"`; coordinated Hermit/Reverie branches live in the
same slot when the change spans both, while unchanged children stay detached at
their parent gitlinks. e9patch behavior validation needs the vendored
`third-party/e9patch/{e9tool,e9patch}` present in the reverie child. Never
feature-build in a primary checkout. See
`ai_docs/transient/2026-07-27-worktree-management-map.md` for the full protocol.

## Related

- [post-facto-review](../post-facto-review/SKILL.md),
  [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md),
  [Hermit debugging](../../../hermit/.claude/skills/hermit-debugging/SKILL.md),
  [progress-rubric](../progress-rubric/SKILL.md), and
  Hermit `repo-cleanliness` skill.
