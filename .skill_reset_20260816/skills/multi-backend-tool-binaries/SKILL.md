---
name: multi-backend-tool-binaries
description: "Historical uncommitted Reverie multi-backend tool exploration; verify current product APIs before reusing any design claim."
---

# Historical multi-backend tool exploration

This note describes uncommitted July 2026 work from an old slot. It is not a
landed API, build target, branch handoff, or current compatibility claim.

The explored shape placed reusable `Tool` logic in one library and thin
per-backend binaries behind Cargo features because ptrace, KVM, and DBI exposed
different runner APIs. DBI baked a tool into its native client rather than
selecting it at runtime; KVM accepted a narrower static-ELF path; ptrace exposed
the general command runner. These are hypotheses to re-check against current
Reverie main, not instructions to recreate old crates or counts.

Any implementation belongs in a registered Reverie feature branch and PR. Trace
the current `Backend`, `Tool`, runner, lifecycle, and RPC call sites; add focused
positive/negative tests; and treat a core Reverie abstraction change as a human
review trigger. Product-specific successor guidance should live in Reverie's
own skill tree, not be expanded in the parent coordinator repository.
