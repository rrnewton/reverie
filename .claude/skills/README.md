# Coordinator skill map

The parent and product repositories have separate skill audiences. Coordinator
skills stay in dev-hermit; product implementation skills stay in the product
repository that owns their code and tests.

## Discovery surfaces

- `.claude/skills/<slug>/SKILL.md` is the one versioned coordinator package.
  The package may also carry `references/`, `scripts/`, or other resources.
- `.agents/skills/<slug>` is a tracked whole-package symlink to that canonical
  directory. `.llms/skills` links to the canonical skill root. Claude, Codex,
  and `.llms` consumers therefore read the same instructions and resources.
- `scripts/check-codex-setup.py` is read-only. It verifies package metadata,
  exact link targets and containment, local Markdown links, the optional
  planner bridge, and the project instruction-size budget.
- `.codex/config.toml` raises `project_doc_max_bytes` so Codex loads the complete
  root-plus-product `AGENTS.md` chain, including the root tail canary.

Edit the canonical package and run the checker. Do not replace a package link
with a generated pointer file, a link to `SKILL.md` alone, or a root-level
`.agents/skills` symlink.

## Active coordinator groups

- Fixed roles: `hermit-ci`, `hermit-coord`, `hermit-dbi`, `hermit-e9patch`,
  `hermit-kvm`, `hermit-lander`, `hermit-liteinst`, `hermit-opt`, and
  `hermit-sabre`.
- Workstream charters: `hermit-linux` (dispatched to a numeric agent and
  generic `slotNN`, not a fixed fleet role).
- Review, planning, reporting: `backend-reality-reviewer`, `benchmarking`,
  `post-facto-review`, `progress-rubric`, and `research-planning-persona`.
- Validation and hosted diagnostics: `validate-orchestrator-discipline`,
  `manual-ci-mode` (the validate-first self-landing protocol), and focused
  historical references.
- Safety references: feature-base, worktree, stash, pinned-branch, syscall, and
  record/replay skills.

The optional `pr-landing-planner` directory remains canonical in
`agent-utils`. Its Claude compatibility symlink is retained, but the stale
current pin is quarantined from new stock-Codex discovery until an isolated
agent-utils pin update is semantically reviewed and passes
`check-agent-utils-pin`. Do not copy the planner into this repository to hide a
stale or missing tooling pin; use ci-hub's tracked planning/status entrypoints.

## Authority and local memories

`AGENTS.md` is policy. Skills provide task workflows and routing; a stale skill
cannot override the canonical policy or an executable semantic verifier.

Some skills have mirrors in an operator-local Claude memory directory.
That bridge is optional and unversioned. It cannot import into or delete
repository skills. The linter gates repository structure and reports local
drift only as advisory; explicit `--adopt-skill` exports a reviewed repository
skill. See `core-memory-skill-sync-tooling` before maintaining that bridge.

## Validation and GH Actions implementation home

Current validation and GH Actions code, live queries, history, runner operations, validation receipts,
and landing mechanics live under `ci-hub/`. Skills describe when and why to use
those authorities; they should not duplicate a flag-by-flag command manual that
can drift. Generic engines stay in the pinned `agent-utils` submodule and are
reached through the parent adapters.

Parent `AGENTS.md` is authoritative under the owner's 2026-08-10 decision:
VALIDATE is the normal Hermit landing authority, LEDGER is its durable record,
GH ACTIONS is advisory P2, and RUNNER means the machine. A normal author works,
validates, applies the ledger-derived label, lands its own PR, and repeats. The
only Hermit producer exception is a PR modifying `scripts/validate.rs`.

## Agent identity and purpose

New general agents use neutral sequential ids such as `codex-1` or `claude-1`;
task-shaped ids go stale when an agent is reassigned. Do not rename live agents
that already have historical ids. Commits keep the final role/agent/machine
attribution line and add repeatable `Task: <stable-task-id>` trailers so actor
identity and work purpose remain separate, durable facts.

## Product scope

Hermit, Reverie, and LiteInst2 may retain their own product-local skill shapes
for standalone development. LiteInst2 currently has no skill bodies, so there
is nothing to adapt; do not copy a parent role merely to populate discovery.
Do not copy parent coordinator roles, worktree policy, landing policy, or
task-closure rules into a product repository. When working inside a product,
its nested `AGENTS.md` adds build, architecture, and test requirements; the
stricter applicable rule wins.

Archived skills live under `.claude/archived_skills/` and are not part of normal
discovery.
