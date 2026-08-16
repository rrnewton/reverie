---
name: validate-sh-rr-compat-counter-conflict
description: "Historical Hermit rebase hotspot: derive record/replay compatibility counters from current source after merging both sides; never copy a dated count."
---

# Record/replay compatibility counter rebases

Some Hermit revisions couple an expected-count constant to a program-label
collection. When both main and a feature branch add programs, the collection may
merge while the scalar conflicts. Inspect the exact current
`scripts/validate.rs` before assuming this mechanism still exists.

If it does, preserve the union of intentional entries, derive the count from the
merged collection with the repository's current verifier, and update the scalar
to that derived count. Never choose a conflict side or copy the dated counts in
old task notes. Run the focused structural test, then classify validation and
landability under skill `hermit-validation-authority`; do not assume that a rebase either
preserves or destroys evidence merely because the SHA changed.

Do not infer validity from a label or reactivate historical hosted behavior.
The current local-versus-GitHub boundary and any producer-change bootstrap live
only in skill `hermit-validation-authority`. This product fact belongs in Hermit and should
migrate there when its current implementation is confirmed.
