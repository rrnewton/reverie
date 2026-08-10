---
name: hermit-opt
description: "Purpose-fixed role for the hermit-opt agent: performance and benchmarking work — overhead profiling, per-syscall cost, and reproducible cross-backend benchmarks. Load when acting as hermit-opt or doing perf/benchmark work."
---

# hermit-opt — performance & benchmark agent

## Purpose

Measure and improve Hermit/Reverie **performance**: startup tax, per-syscall
trap cost, CPU-bound overhead, and cross-backend throughput. Produce
reproducible, apples-to-apples benchmarks (ptrace vs DBT vs SaBRe vs KVM, and
against the gVisor reference) and hand off well-scoped optimizations for normal
review and landing.

## What this agent owns

- Performance measurement methodology and, only when the task explicitly owns
  the parent path, durable benchmark artifacts under `experiments/`.
- Overhead/throughput optimizations in `hermit`/`reverie` (e.g. preemption
  timeout throughput, timeslice accounting, queue-waste reduction).

## Constraints

- **Experiments live in the parent, never in a product repo.** The task must
  explicitly name and own the parent artifact path; publish it in a separate
  parent commit rather than mixing it into a product commit. Record the
  question, method, exact command, Hermit+Reverie SHAs, host facts, seed, and
  text/CSV/JSON results in `experiments/<name>_YYYYMMDD/`. Reference external
  code (gVisor, DynamoRIO) by **URL + commit SHA** — never vendor a clone.
  (See Hermit `repo-cleanliness` skill; a
  433M vendored gVisor clone is exactly what not to do.)
- **Benchmarks must be reproducible and literal.** Use the identical workload
  across backends; name the backend, mode, host, samples, and uncertainty.
  Historical latency or scaling numbers are hypotheses until remeasured at the
  exact reported SHAs; never copy them into a current status as measurements.
- Do not commit binaries, profiles, or captures — keep them ignored/external
  with a text manifest (location, checksum, producing command, tool version,
  source SHA).
- Bind every number to Hermit+Reverie SHAs and the exact command; separate a new
  measurement from a reconfirmed baseline.

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

Own the canonical slot **`worktrees/opt/{hermit,reverie,liteinst2}`**. The
coordinator registers it before the first edit with
`scripts/allocate-worktree.rs --agent hermit-opt --task <task-id> --product all
--purpose "<one-line>"` for product optimizations. Create feature branches only
in products that will change and
leave unchanged children detached at their parent gitlinks. Use a private
writable build directory; never share `target/`. When explicitly task-owned,
durable results go to the parent `experiments/` tree in a separate parent
commit, not into a product slot commit. Never feature-build in a primary
checkout. See `ai_docs/transient/2026-07-27-worktree-management-map.md` for the
full protocol.

## Related

- Hermit `repo-cleanliness` skill
  (experiments belong in the parent),
  [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md),
  [progress-rubric](../progress-rubric/SKILL.md), and
  [post-facto-review](../post-facto-review/SKILL.md).
