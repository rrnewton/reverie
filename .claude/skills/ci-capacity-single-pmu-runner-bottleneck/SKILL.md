---
name: ci-capacity-single-pmu-runner-bottleneck
description: "Historical PMU-runner capacity context. Use only to recognize the old incident; query live runner capacity and exact-head VALIDATE/LEDGER state through ci-hub."
---

# Historical PMU capacity incident

In July 2026, Hermit's host-dependent jobs queued behind scarce PMU runners and
later hit a separate package-provisioning failure. Those runner counts, queue
depths, workflow names, and failure classifications are historical observations,
not current runner or validation truth.

Run `ci-hub/ci-hub quickstart`, then use its current runner-health and PR-status
entrypoints. Verify the running mechanism and exact head; do not infer current
capacity from this note. PMU/KVM work may still require scarce host facilities,
so derive safe concurrency from live capacity before dispatch.

Normal Hermit landing requires `ci-hub validate-status` over a clean, counted
exact-head LEDGER receipt. A `locally-validated` label is only its derived
cache. Runner scarcity and GH Actions queue state are capacity diagnostics, not
landing verdicts. Only a PR modifying `scripts/validate.rs` uses its registered
exact-head GH Actions result because that producer cannot vouch for itself.
