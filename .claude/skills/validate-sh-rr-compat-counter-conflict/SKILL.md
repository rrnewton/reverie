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

Landing authority remains the owner-authorized exact-head OR predicate:
`ci-hub validate-status` for a qualifying local receipt or `ci-hub hosted-status`
for the registered hosted job set. Do not re-fire historical workflows or infer
validity from a label. This product fact belongs in Hermit and should migrate
there when its current implementation is confirmed.

Until
[`hermit-merge-gate-authority-deployment`](../../../ci-hub/landing/README.md#deployment-obligation-hermit-merge-gate-authority-deployment)
lands in Hermit, its required merge-gate still requires portable+privileged and
pins the older verifier. Obey that gate; portable-only hosted authority is not
operational end to end yet.
