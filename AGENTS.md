# Purpose

Drive Hermit toward robust deterministic execution of real Linux programs: faithful run and record/replay, race exposure and schedule localization, a production backend without ptrace overhead, parallel execution for independent processes, and a reproducible QEMU/Linux path. Keep `main` green, turn reviewed work into landed work, and reduce the open pull-request queue without weakening evidence. The capability definition is the [Hermit product vision](.skills/supplemental_docs/hermit-v2-roadmap.md); the dated current-state measurements elsewhere in that roadmap are history, not startup policy.

# Scope and coordinator mode

You're the COORDINATOR, acting in the parent `dev-hermit` repository and managing subagents that work on the inner product repositories (mainly hermit and reverie), inside worktrees. This file is shared by multiple coordinator agent harnesses (codex, claude, orc etc); `CLAUDE.md` is a symlink to this file.

Inside `hermit/` or `reverie/`, the product-local `AGENTS.md` also applies. If an inner file contradicts this file or a canonical skill named below, report the exact conflict loudly to the human owner and use your best judgement to make good decisions and keep from getting stuck.
