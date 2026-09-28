---
name: hermit-lander
description: "Compatibility role pointer for the Hermit landing agent. Load when planning or executing PR landings; it delegates planning rules to pr-landing-planner and execution rules to the tracked ci-hub lander."
---

# Hermit lander

This role has no independent landing protocol.

1. On startup or replacement, run `ci-hub/ci-hub inherit-obligations --agent
   <registered-agent>` and inspect every inherited remediation before accepting
   new queue work. Then load
   [pr-landing-planner](../pr-landing-planner/SKILL.md) and follow its canonical agent-utils skill before
   choosing, ordering, or assigning a landing batch.
   Publish each PR-to-agent assignment on the pull request itself: set its
   `Owner:` line and post a comment (see the live `AGENTS.md`). Under ORC the
   dispatch task may point at that comment. Comments are pull-based; for a
   time-sensitive ready-to-land handoff, ask the coordinator to relay after
   writing the comment. Do not use
   agent-side `SendMessage` with an ORC fleet name or report an attempted send
   as delivery.
2. **Follow skill `hermit-validation-authority` for validation authority and evidence.** The author normally lands its own reviewed PR.
   Use this dedicated role only for inherited backlog recovery. Execute a
   task-authorized Hermit landing with `ci-hub/landing/land-pr.sh`;
   pass the
   registered agent through `--agent` and set `LANDER_MODEL` to the actual model
   identity, whether the lander is a Claude, Codex, or other client. Never rely
   on a client-specific default. That tracked program owns the land lock,
   selected validation predicate, fresh-base handling, merge mode, and ancestry
   check; this compatibility role does not restate those rules.
3. Follow `AGENTS.md` for review, publication, and task-closure policy.

Do not add substantive landing rules here. Update the canonical planner skill or the executable that
enforces the rule, then keep this file as a thin role pointer.
