---
name: validate-orchestrator-discipline
description: "Coordinator reporting and debugging discipline around Hermit validation. Use hermit-validation-authority for the producer path, validation authority, evidence fields, hard/soft-green landability, red-main repair, and the GitHub Actions boundary."
---

This skill adds coordinator reporting and debugging discipline. It does not
define validation authority or landing eligibility. Follow
[`hermit-validation-authority`](../hermit-validation-authority/SKILL.md) for the sole production path,
the LEDGER record and its fields, hard/soft-green landability, red-`main`
repair, and the current GitHub Actions boundary.

1. **Report the durable run, not a remembered outcome.** Green qualification is
   defined only by `hermit-validation-authority`. In handoffs, state the run handle, durable
   log path, and SHA so another agent can dereference the same evidence.

2. **Use the canonical durable producer because agents recycle.** The command
   and producer boundary live only in `hermit-validation-authority`. Hand off the emitted
   handle rather than relaunching an existing run.

3. **When `main` is already red, tight-loop the failing validation node.** A
   direct `scripts/validate.rs --only <node>` or `ci/run-node.sh` invocation is
   a debugging probe only: it does not write a qualifying LEDGER record and
   cannot authorize landing. Use it to shorten the edit loop, then follow the
   red-`main` repair and production-validation path in `hermit-validation-authority`.

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
   is ON by default; DAG nodes were observed lacking `cpu_timeout` on
   2026-08-03, but a `cpu_timeout` budget and its enforcement have since been
   added — reconfirm current node coverage rather than assuming either state).
   Report the delta between the stated directive and the verified state.

See [compatibility-counter rebases](../validate-sh-rr-compat-counter-conflict/SKILL.md)
and the [historical landing alias](../pr-landing-mechanics-merge-gate-uptodate-chase/SKILL.md)
for adjacent context. Query ci-hub and the running cgroup for current runner
boxing and timeout state; do not trust a dated skill claim.
