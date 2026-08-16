---
name: undraft-does-not-trigger-ci
description: "Historical workflow-trigger observation: ready-for-review does not create validation authority. Inspect current workflow behavior as diagnostics and use hermit-validation-authority for Hermit validation and landing."
---

# Undrafting is not a validation event

An older Hermit workflow omitted `ready_for_review`, so undrafting did not start
GH Actions. Workflow triggers can change. Inspect the workflow at current main before
making a queue claim, and never use draft status or a trigger assumption as
validation evidence.

Undrafting creates no validation evidence. Follow skill `hermit-validation-authority` for
current authority, exact-SHA evidence identity, hard/soft green, and the hosted
boundary.
