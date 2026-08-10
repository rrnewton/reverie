---
name: manual-ci-mode
description: "Hermit's validate-first landing workflow. Use for any Hermit PR publication or landing: local VALIDATE recorded in the LEDGER is the normal authority; GH Actions is advisory P2 except for a PR modifying scripts/validate.rs."
---

# Validate-first Hermit landing

Parent `AGENTS.md` is authoritative under the owner's 2026-08-10 decision. Use
its vocabulary exactly: VALIDATE is the admitted local full run; LEDGER is its
durable authority record; GH ACTIONS are hosted advisory P2 diagnostics; RUNNER
is the machine. Never use the ambiguous two-letter continuous-integration
abbreviation for any of those meanings.

The ordinary inner loop is:

1. Work in the assigned slot and publish the reviewed PR at an exact head.
2. Run full VALIDATE through `ci-hub validate-run`.
3. Require `ci-hub validate-status --sha <40-hex-head>` to accept a clean,
   full, nonzero, counted LEDGER receipt.
4. Let `ci-hub apply-local-label` derive `locally-validated` from that receipt.
5. Land your own PR with `ci-hub/landing/land-pr.sh`, prove ancestry, and repeat.

Do not wait for GH Actions, a merge-gate run, or a dedicated landing agent.
Those signals remain useful diagnostics, but they neither authorize nor veto a
normal ledger-authorized Hermit landing. If the land breaks `main`, stop the
drain and re-green it rather than accumulating a backlog.

Admit VALIDATE through ci-hub and the tracked `systemd-run --user` producer in
`AGENTS.md`, from the assigned slot. Require a clean anchored 40-hex commit,
full profile and selection, nonzero counted coverage, zero failures, and a
durable log/LEDGER row. Any rebase or head change requires a new VALIDATE.

`locally-validated` is only a cache derived from the receipt. Never type it by
hand or treat its presence, a command exit, a comment, or copied status as
authority. The tracked lander owns final authority recheck, fresh-base handling,
serialization, merge mode, and ancestry proof. Do not use `--admin`, a primary
checkout, or direct `./scripts/validate.rs` from an agent sandbox.

## Sole Hermit producer exception

A PR that modifies the registered producer `scripts/validate.rs` cannot use a
run produced by its own changed code as authority. Only that narrowly detected
PR class uses the registered exact-head GH Actions result through `ci-hub
hosted-status`. Do not extend the exception to adjacent validation files,
normal PRs, a missing hosted run, or a hosted red on an ordinary head.

## Other repositories select their own authority

`rrnewton/dev-hermit` has no Hermit validation producer and must not append
synthetic rows to the Hermit LEDGER. Its separately versioned policy currently
uses the registered 4/4 exact-head GH Actions job set. Reverie likewise retains
its separately versioned hosted policy. Neither exception changes normal
Hermit's validate-first loop.
