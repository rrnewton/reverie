---
name: validate-orchestrator-discipline
description: "Coordinator discipline for Hermit validation: require an exact-SHA durable log, launch full runs through systemd-run --user, tight-loop focused failures without bypassing review or landing gates, and verify measurements and running mechanisms."
---

Coordinator/orchestrator discipline for running `validate` and reporting its
results. Every rule below exists because the coordinator VIOLATED it on
2026-08-03; they bind coordinator behavior, not just the tool. The TOOL's own
invariants are the Hermit-level skill, proposed at
`ai_docs/validate-invariants-hermit-skill-proposal.md` (to be placed by the owner
under `hermit/.llms/skills/`). Verified 2026-08-03 against `worktrees/226v/hermit`,
the tool ENFORCES: HEAD-SHA recording into a JSONL run ledger (there is NO
dirty-tree gate and NO `--run-on-dirty-tree` flag); fail-open-to-FULL
affected-test selection (`!= 'false'` job gating — default is selective only in
portable CI, but FULL inside `scripts/validate.rs` itself); wall-clock process-tree gate
timeouts; flaky-is-RED trinary stress classification; history-derived cost
estimates always-printed on every exit path (EXIT trap); honest count ratchets
(`RR_COMPAT_EXPECTED=139` etc. guarded against the actual array size); and a
four-layer architecture (`scripts/validate.rs` → `ci/run-dag.sh` → `safe-ci-dag-runner`
→ shared TOML manifests, determinism via `hermit --verify`, not bash). It does
NOT enforce: any CPU-time / `RLIMIT_CPU` budget (all timeouts are wall-clock),
any performance ratchet (profiling CSVs are observability-only), or a dirty-tree
refusal. **There is NO symlink sharing between `dev-hermit/.claude/skills` and
`hermit`'s skill dirs** — this cross-reference is an EXPLICIT LINK, not
inheritance; read both.

1. **Never claim green without a completed, durable log.** "Green" requires a
   validation run that actually FINISHED and left a durable, timestamped log you
   can point to (per-node wall+CPU, the validated SHA, pass/fail per cell). A
   still-running run, a killed run, a run whose log you cannot produce, or a
   remembered outcome is NOT green. State the log path and the SHA, or do not say
   green.

2. **Run validate through the durable producer path, because agents recycle.**
   A full validate outlives an agent's context window. Use the `systemd-run
   --user` form in `AGENTS.md`, with an explicit worktree, exact SHA, unit name,
   and durable log. Direct sandbox execution and ad-hoc `nohup` jobs do not
   establish the required cgroup/producer binding. Hand off the unit, log path,
   and SHA so a replacement can verify completion.

3. **When main is already RED, tight-loop the failing cell without weakening
   publication gates.** Iterate on the specific failing node (`scripts/validate.rs
   --only <node>` / `ci/run-node.sh`) rather than rerunning the whole DAG after
   every edit. Before publication or landing, still satisfy the task's review,
   exact-head receipt verification and landing-lock requirements. A
   red baseline changes debugging order; it does not authorize an unreviewed or
   unverifiable merge.

4. **Interrogate a surprising ratio; never just route it.** An unexpected
   pass/fail ratio, a suspiciously round number, or a result that contradicts a
   baseline is a signal to DIG (which cell, which SHA, is it flaky, is the
   selector fail-open masking skips), not a workload to hand to another agent or
   summarize away. Flaky (any result strictly between 0% and 100%) is RED, not a
   footnote.

5. **Never present a fabricated number as a measurement.** An estimate, a guess,
   a "should be ~N", or a hardcoded range is not a measurement and must never be
   reported as one. Derive costs/counts from history or an actual run, label
   estimates as estimates, and print ACTUAL wall+CPU on completion including
   failure paths. A number with no run behind it is a lie.

6. **Verify a repeatedly-stated architectural directive is actually in effect.**
   When the owner has said something "should be so" more than once (e.g. cgroups
   on, CPU-time timeouts set, the runner orchestrating), do not assume it is
   true — grep the call sites and read the config, because directives drift from
   reality (e.g. `--cgroups` was accepted-but-inert for months before it was
   removed — it now hard-errors with exit 2 in both engines, and cgroup-v2 boxing
   is ON by default; ALL DAG nodes lack `cpu_timeout`).
   Report the delta between the stated directive and the verified state.

See [compatibility-counter rebases](../validate-sh-rr-compat-counter-conflict/SKILL.md)
and the [historical landing alias](../pr-landing-mechanics-merge-gate-uptodate-chase/SKILL.md)
for adjacent context. Query ci-hub and the running cgroup for current runner
boxing and timeout state; do not trust a dated skill claim.
