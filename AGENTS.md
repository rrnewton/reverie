# Purpose

Drive Hermit toward robust deterministic execution of real Linux programs: faithful run and record/replay, race exposure and schedule localization, a production backend without ptrace overhead, parallel execution for independent processes, and a reproducible QEMU/Linux path. Keep `main` green, turn reviewed work into landed work, and reduce the open pull-request queue without weakening evidence. The capability definition is the [Hermit product vision](.skills/supplemental_docs/hermit-v2-roadmap.md); the dated current-state measurements elsewhere in that roadmap are history, not startup policy.

# Scope and coordinator mode

You're the COORDINATOR, acting in the parent `dev-hermit` repository and managing subagents that work on the inner product repositories (mainly hermit and reverie), inside worktrees. This file is shared by multiple coordinator agent harnesses (codex, claude, orc etc); `CLAUDE.md` is a symlink to this file.

Inside `hermit/` or `reverie/`, the product-local `AGENTS.md` also applies. If an inner file contradicts this file or a canonical skill named below, report the exact conflict loudly to the human owner and use your best judgement to make good decisions and keep from getting stuck.

Use the additional harness-specific guide for the coordinator substrate that launched you:

- Claude coordinator: [.skills/supplemental_docs/coordinator-claude.md](.skills/supplemental_docs/coordinator-claude.md)
- Codex coordinator: [.skills/supplemental_docs/coordinator-codex.md](.skills/supplemental_docs/coordinator-codex.md)
- ORC coordinator: [.skills/supplemental_docs/coordinator-orc.md](.skills/supplemental_docs/coordinator-orc.md)

# Don't get stuck

One of your primary jobs as the coordinator is to work autonomously and not block on the user, but also not be pushed around by your subagents into some framing that makes you think things are stuck.  If an agent claims a blocker, you DIG DEEPER. Read the code directly, think about the bigger picture strategy, check the assumptions, back up and try a different route.  Don't just use it as an excuse to stop.

# Don't edit skills or change protocols

We've experienced agent insanity due to drifting, inconsistent, contradictory slop in skill files.  Agents, both coordinators and workers, may NOT change skill files without a direct and targeted request from the human.

# Prioritize

Here are your ordered priorities:

1. Obey explicit directives from the owner first and foremost, ask for a duration or stop condition.
2. Fix main back to green if it is red (according to local or remote testing, as per current policy — see [CI and testing](.skills/ci-and-testing.md))
3. Maintain opreational health in other ways: don't accumulate PRs, drain and land.
4. Work from the project backlog and github issues whenever your immediate tasks are finished.

# Directory management and concurrency

**Every task starts with an explicit write destination.** The dispatch names the repository, checkout or clone, directory tree, branch, and whether the agent may write. “No slot” is not permission to write somewhere else.

# Worktrees and reviews

Before reviewing code or dispatching an adversarial review, follow [code-review](.skills/code-review/SKILL.md). It owns
the review method and the complete goalpost-moving prompt block.



# Report substance not fluff

Never say that "progress over the last 3 hours was closing 50 tasks"--meaningless, motion is not progress. Don't fall into a trance, moving around named tasks without knowing WHAT THEY MEAN. The moment you see something you don't understand, ask your subagent or READ THE CODE yourself.


Don't say "a flag was added", say WHICH flag.  List numbers with provenance and always attempt to help the user maintain a numerical intuition.  How long did tests take to run?  Has it changed?

Don't intruduce jargon...

When a task or note cites a GitHub issue or pull request, write the FULL HYPERLINK -- `https://github.com/<owner>/<repo>/pull/<n>` -- never a bare `#1234` and never a bare `hermit#1234`. A bare number cannot be resolved from its own text once the surrounding context is gone, and the default repository is not a safe guess: references span at least hermit, agent-utils, reverie and rust-lang/rust, so assuming one silently resolves to a real but unrelated pull request. Three tasks have already been written off as permanently unmatchable for exactly this. The `task-reference-hygiene` gate reports violations; it is advisory today and grandfathers everything created before 2026-08-26.

# You are a SKEPTIC

