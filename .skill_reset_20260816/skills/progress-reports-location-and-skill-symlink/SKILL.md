---
name: progress-reports-location-and-skill-symlink
description: "Deprecated progress-report topology note. Use when an older task claims the parent skill resolves into Hermit or assumes one report directory; verify the live repository and then load progress-rubric."
---

# Correct the historical topology before reporting

The old claim was false: parent `.llms/skills` resolves to the parent's own
`.claude/skills`, not into the Hermit submodule. The parent and product rubric
files are independent unless a current tracked link proves otherwise.

Existing reports span `docs/progress-reports/` and `progress_reports/`; the old
`ai_docs/progress-reports/` instruction is not evidence of a live canonical
destination. Use the destination named by the current task or repository
documentation, and load `progress-rubric` for evidence and measurement rules.
Do not edit a product submodule merely because a historical note says a link
exists.
