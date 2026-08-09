---
name: validate-sh-cannot-be-green-on-devserver
description: "Retraction: full Hermit validation can pass on a development server; judge each exact-head run and host-sensitive failure from its durable evidence."
---

# Retraction: development hosts have no structural red floor

The old claim that full Hermit validation cannot pass on a development server was
disproved on 2026-08-03 by a complete five-gate green run. Never assume a host is
intrinsically red or green.

Launch a full run through the `systemd-run --user` producer in `AGENTS.md`, from
the assigned slot. Require its exact 40-hex SHA, clean/anchored state, full
profile and selection, counted coverage, zero failures, durable log, and ledger
row. `ci-hub validate-status --sha <head>` is the local authority verifier; the
owner-authorized hosted alternative is `ci-hub hosted-status`. The raw exit code
and `locally-validated` label are not authority.

Until
[`hermit-merge-gate-authority-deployment`](../../../ci-hub/landing/README.md#deployment-obligation-hermit-merge-gate-authority-deployment)
lands in Hermit, its required merge-gate still requires portable+privileged and
pins the older verifier. Obey that gate and do not report portable-only hosted
authority as deployed end to end.

Some PMU/instruction-sensitive tests can fail on particular hosts. Prove an
environmental classification with the same focused test on clean current main,
report both SHAs and outputs, and never weaken or strip the test to create a
green result. See [validation orchestration](../validate-orchestrator-discipline/SKILL.md).
