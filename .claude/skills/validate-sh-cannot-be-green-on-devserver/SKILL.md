---
name: validate-sh-cannot-be-green-on-devserver
description: "Retraction: full Hermit validation can pass on a development server; judge each exact-head run and host-sensitive failure from its durable evidence."
---

# Retraction: development hosts have no structural red floor

The old claim that full Hermit validation cannot pass on a development server was
disproved on 2026-08-03 by a complete five-gate green run. Never assume a host is
intrinsically red or green.

Follow skill `hermit-validation-authority` for the producer path, green qualification, and
current local-versus-GitHub boundary. This retraction establishes only that a
development host has no structural red floor.

Some PMU/instruction-sensitive tests can fail on particular hosts. Prove an
environmental classification with the same focused test on clean current main,
report both SHAs and outputs, and never weaken or strip the test to create a
green result. See [validation orchestration](../validate-orchestrator-discipline/SKILL.md).
