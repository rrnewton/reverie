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
old task notes. Run the focused structural test and then obtain a new full
exact-head receipt because a rebase changes the SHA.

Normal Hermit landing authority remains the exact-head LEDGER receipt accepted
by `ci-hub validate-status`. Do not re-fire historical GH Actions or infer
validity from a label. Because this conflict may modify the registered producer
`scripts/validate.rs`, classify the final diff precisely: only when that file
changes does the narrow producer exception require registered exact-head GH
Actions authority. This product fact belongs in Hermit and should migrate there
when its current implementation is confirmed.
