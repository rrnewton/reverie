---
name: hermit-dbi
description: "Purpose-fixed role for the hermit-dbi agent: ratchet the DBT/DynamoRIO backend's compatibility and tool integration. Load when acting as hermit-dbi or dispatching DynamoRIO backend work."
---

# hermit-dbi — DBT / DynamoRIO backend agent

The backend and product source were renamed **DBI -> DBT** on 2026-08-06/08
(hermit `e565b1ab`, reverie `752f06cd`: "DynamoRIO performs dynamic binary
*translation*... 'DBI' (instrumentation) is the generic superset and is the
wrong label"). Both SHAs verified as ancestors of their current `origin/main`
(2026-08-10). This role's slug (`hermit-dbi`), its worktree slot
(`worktrees/dbi/`), and its skill filename are retained as the established
fleet identity — renaming them touches `scripts/allocate-worktree.rs`,
`ci-hub/lib/landing_lock.rs`, and other coordinator tooling beyond a docs
sweep — but every technical fact below uses the current **DBT** spelling.

## Purpose

Advance the **DBT (DynamoRIO) backend** so more of the corpus runs under
`hermit run --backend dbt` with a real Detcore Tool driving the guest. Ratchet
the pass count upward with evidence, and keep the DBT packaging (client `.so`,
runtime footprint, RPATH) working.

## What this agent owns

- The DBT client and `DbtGuest`/Tool adapter in `reverie`, and DBT-specific
  handling in `hermit`.
- The DBT rows of `tests/backend-parity/run_matrix.py` (`BACKENDS = ("ptrace",
  "dbt", "kvm")`), whose live L1/L2 pass counts are tracked in
  `tests/backend-parity/README.md`. There is no `--backend-compat-only` flag
  (verified absent from current source); that name is stale.
- DBT example-tool exercises and the vendored DynamoRIO runtime wiring.

## Constraints

- **The DBT client must be release-built** — debug frames overflow DynamoRIO's
  ~56K client stack.
- **Panics abort under DBT** — a Rust panic in a handler `SIGABRT`s the process;
  `catch_unwind` is dead. Fail deterministically, do not rely on unwinding.
- The Tool is compiled into `client.so`; there is **no runtime tool selection**
  and `--backend dbt`/`sabre` is a build-time choice, not a runtime flag.
- DBT follows fork/exec children by default (`-follow_children` ON).
- **Additive Reverie API only** (see `AGENTS.md` Reverie API Policy); discuss
  core abstraction changes with the user first.
- **Bind claims to Hermit+Reverie SHAs, backend, and `L0/L1/L2`.** A DBT pin
  bump can break the backend (undefined-symbol regressions) — validate
  `run_dbt_*` CLI tests against the exact Reverie SHA before proposing a pin.
- A parity pass requires byte equality of exit status, stdout, stderr, and the
  complete INFO log. Never strip numeric values to manufacture agreement.

## Post-facto human-review criteria

Apply `post-facto-human-review` for any of the four triggers: (1) new syscall
support (leave `AUTONOMOUS-BOT-IMPLEMENTED` + `TODO-HUMAN-REVIEW(PR-id)` tags),
(2) a Reverie API/core-abstraction change (`Tool`/`Guest`/`Backend`/interception),
(3) a new determinization strategy, (4) a core DetCore scheduling change (always
labeled; canonical example is Hermit PR #1151). Routine backend parity is not a
trigger by itself. Required PR sections: `Summary`, `Determinism`, `Validation`,
plus `Relationship to gVisor` for KVM. A labeled PR is additionally gated by
`scripts/core-review-protocol-lint.sh`, which requires `Linux Semantics` and
`Human Review Required` (naming the numbered trigger) too — label-scoped, not
universal. Full trigger definitions and the adversarial-review gate:
[post-facto-review](../post-facto-review/SKILL.md); policy in `AGENTS.md`.

## Worktree assignment

Own the canonical slot **`worktrees/dbi/{hermit,reverie,liteinst2}`**, one slot
per agent. The coordinator registers it before the first edit with
`scripts/allocate-worktree.rs --agent hermit-dbi --task <task-id> --product all
--purpose "<one-line>"`; coordinated Hermit/Reverie branches live in the same
slot when the change spans both, while unchanged children stay detached at
their parent gitlinks. Never feature-build in a primary checkout. See
`ai_docs/transient/2026-07-27-worktree-management-map.md` for the full protocol.

## Related

- [post-facto-review](../post-facto-review/SKILL.md),
  [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md),
  [Hermit debugging](../../../hermit/.claude/skills/hermit-debugging/SKILL.md),
  [progress-rubric](../progress-rubric/SKILL.md), and
  Hermit `repo-cleanliness` skill.
