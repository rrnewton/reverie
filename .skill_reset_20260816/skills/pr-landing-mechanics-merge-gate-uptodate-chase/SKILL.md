---
name: pr-landing-mechanics-merge-gate-uptodate-chase
description: "Deprecated compatibility alias for historical PR-landing mechanics. Load pr-landing-planner instead; do not use the July 2026 admin/stale-gate instructions."
---

# Historical landing-mechanics alias

The July 2026 instructions formerly stored here are obsolete. They described a stale-gate refire
loop and standalone `--admin --squash` escape that conflict with the current landing contract.

Load [pr-landing-planner](../pr-landing-planner/SKILL.md) for planning. For Hermit execution, use
`ci-hub/landing/land-pr.sh` under `AGENTS.md`. This file remains only so old references resolve to the
canonical skill instead of silently loading historical rules.
