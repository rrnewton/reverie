---
name: undraft-does-not-trigger-ci
description: "Historical workflow-trigger observation: ready-for-review does not create VALIDATE authority. Inspect current GH Actions only as diagnostics and use the LEDGER for normal Hermit landing."
---

# Undrafting is not a validation event

An older Hermit workflow omitted `ready_for_review`, so undrafting did not start
GH Actions. Workflow triggers can change. Inspect the workflow at current main before
making a queue claim, and never use draft status or a trigger assumption as
validation evidence.

Normal Hermit landing uses the exact-head LEDGER receipt accepted by `ci-hub
validate-status`. Undrafting creates no authority, and any head change
invalidates the old receipt. GH Actions remains advisory P2 unless the PR
modifies the registered producer `scripts/validate.rs`.
