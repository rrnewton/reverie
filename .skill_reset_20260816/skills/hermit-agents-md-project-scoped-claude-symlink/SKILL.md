---
name: hermit-agents-md-project-scoped-claude-symlink
description: "Keep product and coordinator policy scopes separate. Use when editing Hermit guidance: hermit/AGENTS.md is product-local and hermit/CLAUDE.md aliases it; dev-hermit/AGENTS.md owns fleet and worktree policy."
---

`hermit/AGENTS.md` is the product developer guide; `hermit/CLAUDE.md` is its
compatibility symlink and must not be edited separately. Parent coordination,
slot ownership, publication, and closure policy belong in
`dev-hermit/AGENTS.md`.

Product documentation changes follow the same registered-slot, feature-branch,
validation, and PR workflow as product code. Never switch or develop in the
Hermit primary, never create an unregistered raw worktree, and never push
directly to Hermit `main` to work around a dirty shared checkout.
