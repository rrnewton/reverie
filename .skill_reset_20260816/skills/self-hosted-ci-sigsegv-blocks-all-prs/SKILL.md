---
name: self-hosted-ci-sigsegv-blocks-all-prs
description: "Historical July 2026 self-hosted GH Actions SIGSEGV incident, not current validation truth. Use only when an old note attributes every PR red to it; re-query current state and apply hermit-validation-authority."
---

# Historical incident; verify current state

In July 2026, a main-level `clone_with_stack` SIGSEGV made many self-hosted
checks fail together, and PR #203 added a narrow known-failure entry. That
incident does not establish the state or authority of any present-day check.

For current work, query `ci-hub/ci-hub quickstart` and
`ci-hub/bin/pr-status`, bind every result to the exact PR head, and distinguish
a reproduced baseline/environmental failure from a product failure. Follow
skill `hermit-validation-authority` for validation authority and landability; never infer
either from an old incident, a label, or a branch-protection snapshot.
