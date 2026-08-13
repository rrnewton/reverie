# Load-bearing authorities: unbound-read audit

**2026-08-13, `hermit-903`, task `audit-load-bearing-authorities-for-unbound-reads`.**
Read-only. No label, approval, receipt, merge, or authorization was created,
edited, or removed anywhere in the course of this audit.

Companion to
[`ai_docs/policy/proxy-binding-authority-registry.md`](policy/proxy-binding-authority-registry.md),
whose twelve-row registry this audit verifies against live code. **This file
records findings; the registry remains the maintained artifact.** Rows this
audit found stale are listed under *Registry maintenance* with the exact
correction, so the registry is updated from code rather than from prose.

---

## The one question

For each authority: **what does it CLAIM to assert, what does it ACTUALLY read,
and are those the same thing?**

## Method, and a correction to my own first metric

I began by counting non-test callers of each canonical verifier as a
blast-radius proxy:

| authority | verifier | call sites |
| --- | --- | --- |
| validation ledger | `validate-status` | 22 |
| worktree registry | `check-worktree-registry` | 12 |
| receipt verification | `verify_receipt.sh` | 8 |
| GH result classifier | `check_outcome.py` | 8 |
| reverie pin | `check-reverie-pin` | 8 |
| landed identity | `verify-landing` | 5 |
| review binding | `approval_binding.py` | 4 |
| conflict graph | `pr_conflict_graph.py` | 1 |
| closure gateway | `verified_close.py` | 1 |

**That metric is itself an unbound read, and it misled me on the first row I
checked.** A call-site count claims "this many consumers are bound by the
verifier" and actually reads "this many files mention its name". It cannot
distinguish a gate from a log line. `scripts/release-worktree.rs` matched twice
and looked like an upgrade from the registry's recorded `PARTIAL`; reading the
call sites showed one is a hard pre-check and the other runs **after** the
release and prints `(advisory, not a failure)`. The count was right and the
conclusion it invited was wrong.

Counts below are therefore reported as *mentions*, and every status claim rests
on reading the call site. Where I did not read a call site I say so.

---

## Findings

### A. Registry rows that are STALE — recorded status no longer matches code

**A1. Adversarial/human review — recorded `MISSING`, actually `PARTIAL`.**
The registry says "**None.** `scripts/core-review-protocol-lint.sh` checks cache
labels only." A canonical dereferencing verifier now exists and is landed and
activated: `ci-hub/health/approval_binding.py`, whose `lane_shas()` +
`classify_approval()` bind a lane to an exact 40-hex head. Three non-test
consumers dereference it — `ci-hub/landing/land-pr.sh` (at its final mutation
boundary), `ci-hub/health/pr_status.py`, `ci-hub/landing/pr_head_ci.py`.

The hole is now specific rather than total: `hermit/scripts/core-review-protocol-lint.sh`
on `main` contains **zero** occurrences of `PR_HEAD_SHA` — it is still
label-only, so a merge performed through GitHub is unbound while a merge through
`land-pr.sh` is bound. hermit#2216 closes it and is unlanded.

**A2. Live dependency currency — recorded `PARTIAL until #1591 lands`, now landed
with the named consumers present.** `hermit/scripts/check-reverie-pin.rs` exists,
and `reverie_pin` appears as a node in **both** `hermit/ci/dag/portable.json` and
`hermit/ci/dag/privileged.json`, plus `hermit/ci/run-reverie-pin-check.sh`,
`hermit/ci/test_harness.sh`, `hermit/scripts/lib/validate_runtime.rs` and
`hermit/.github/workflows/merge-gate.yml`. Local validate, both DAGs and the
merge gate — three of the five named required consumers — are demonstrably
present. **I did not verify "hosted aggregate" or "receipt production"**, so the
row should move to a narrowed `PARTIAL` naming only those two, not be marked
covered.

### B. Registry rows CONFIRMED accurate — the audit found no drift

Reported explicitly, because a clean finding is only worth something if the
auditor was willing to report a dirty one.

**B1. Landed PR identity and task closure — `PARTIAL`, accurate as written.**
`ci-hub/landing/land-pr.sh:539-541` does reimplement the proof
(`gh pr view --json mergeCommit` then `merge-base --is-ancestor ... origin/main`)
instead of calling `verify-landing`. Worth stating precisely: **the
reimplementation performs the correct check.** The defect is duplication, not
incorrectness — the same divergence risk as any second copy of a grammar.

> **RETRACTED 2026-08-13 — this section originally named
> `ci-hub/remediation/land_and_arm.py:138` as "the real unbound read" and the
> ranking below placed it second by blast radius. That was wrong. See
> *Retraction* at the end of this document; the claim is withdrawn, not
> softened.**

**B2. Merge conflicts and final mergeability — `COVERED`, accurate.** Planning
fetches refs and runs real merges; the actual merge command is the final
arbiter; `mergeable`/`mergeStateStatus` are used as hints.

**B3. Landing authorization and mechanism overlap — `MISSING`, accurate.** Both
correctly record that no verifier exists and that a human/coordinator judgement
is required. Recording an absence as an absence is the correct entry.

### C. New instance found by this audit

**C1. `verify_registry_advisory` reads a missing verifier as "no drift".**
`scripts/release-worktree.rs:6199-6203`:

```rust
fn verify_registry_advisory(root: &Path) {
    let checker = root.join("scripts/check-worktree-registry.rs");
    if !checker.exists() {
        return;                     // silence, indistinguishable from "clean"
    }
```

Claims: the registry was checked after release. Actually reads: the registry was
checked **if the checker happened to be present**. Absence of the verifier and
absence of drift produce identical output. This is worked example #6 ("a green
result with no executed count") in a new place, and it is the same shape as
instance 9 in the task brief — a gate that cannot speak emitting silence rather
than its inability to speak.

Low blast radius: one caller, post-action, explicitly advisory, and a hard
pre-check (`verify_registry_scope`) already gates the acting path. Reported for
completeness, not as a landing blocker.

---

## Ranking by blast radius

Blast radius here means **how many decisions trust the wrong answer**, judged
from read call sites, not from mention counts.

| rank | authority | unbound read | who trusts it |
| --- | --- | --- | --- |
| 1 | **Review, on the GitHub merge path** | label presence, no SHA | every `post-facto-human-review` PR merged through GitHub rather than `land-pr.sh`. Fix exists (hermit#2216), unlanded. |
| ~~2~~ | ~~**Landed identity in `land_and_arm.py`**~~ | ~~`state == MERGED` + SHA *shape*~~ | **RETRACTED — not a defect. See *Retraction*.** |
| 3 | **Landing authorization** | nothing — no verifier | every land. Currently held by human judgement, correctly recorded as `MISSING`. |
| 4 | **Historical Git provenance** | 40-hex syntax, no dereference | every measurement claiming a commit. |
| 5 | **Workflow-policy version** | required-context name | every green emitted by a stale branch YAML. |
| — | `verify_registry_advisory` | verifier presence | one post-action advisory. |

Ranks 3–5 are unchanged from the registry and are recorded there; this audit
confirms them rather than re-deriving them.

---

## The class, stated once

Every instance in the brief and every row above reduces to the same substitution:
**a property that is cheap to read stands in for a property that is expensive to
read, and nothing observably binds them.** Label presence for head identity.
`MERGED` for ancestry. A name for a version. File presence for a check having
run. Mention count for enforcement — including mine, above.

The symmetry matters. A false green admits bad work; a false red teaches the
fleet to explain away real failures as "probably infra". Both erode the same
thing, and the second is harder to notice because it produces no incident.

Two cures already written down and both load-bearing:

1. **Carry the condition with the value** (registry, *The Generative Cure*).
2. **One verifier per authority, called by every consumer** — where "called"
   must mean *gates*, not *mentions*, which is exactly what my first metric got
   wrong.

To which this audit adds a third, from C1 and from instance 9:

3. **A check that cannot run must say so.** Returning silence when the verifier
   is absent makes "not checked" and "checked, clean" the same observation.
   Inability to speak is a third state and must be emitted as one.

---

## What I did not do

- I did not verify the "hosted aggregate" or "receipt production" consumers of
  the reverie pin, so A2 stays `PARTIAL`.
- I did not re-test instances 2, 3, 4, 6-8 from the brief (verify-allow,
  e2e cell promotion, DBT `--verify-json`, the three infra false-reds). They are
  owned elsewhere and I did not want to duplicate or pre-empt that work; they
  are recorded here as the same class on their owners' evidence, not mine.
- I ran no bracket of my own in this audit, because it is an audit and every
  authority examined either already carries brackets in its own test suite or is
  recorded as having no verifier to bracket. Where I changed a status I did it
  from reading the code, and I have said which call sites I read.

## Registry maintenance — exact corrections to apply

1. **Adversarial/human review**: verifier `None` → `ci-hub/health/approval_binding.py`
   (`lane_shas` + `classify_approval`). Coverage `MISSING` → `PARTIAL`: covered on
   `land-pr.sh`, `pr_status.py`, `pr_head_ci.py`; **MISSING on the GitHub merge
   path** because `hermit/scripts/core-review-protocol-lint.sh` is still
   label-only on `main` (hermit#2216 pending).
2. **Live dependency currency**: drop "until #1591 lands" — it is landed. Narrow
   the remaining hole to the hosted aggregate and receipt-production consumers.
3. **Worked examples**: add C1, "a verifier whose absence is reported as a clean
   result", as a thirteenth example.

These are recorded here rather than applied in the same change, so the registry
edit is reviewable against this evidence instead of arriving with it.

---

## Retraction — `land_and_arm.py` is not an unbound read

**2026-08-13, same author, task `land-and-arm-shape-for-ancestry-substitution`.**

This audit named `ci-hub/remediation/land_and_arm.py:138` as "the real unbound
read" and ranked it second by blast radius. **That claim is withdrawn.** It was
established by reading a predicate; testing the code refutes it.

`observe_merged_sha`'s `state == "MERGED" and SHA_RE.fullmatch(sha)` is a
**polling pre-filter** deciding when to stop waiting on GitHub. It is not the
authority. The authority is one call deeper and it is bound:

```
land_and_arm.arm_sha -> protocol.main(["arm", sha, ...]) -> arm()            [protocol.py:5364]
  -> resolve_landed_sha()                                                     [protocol.py:5385]
     -> _fetch_target(source, "main")  then  _is_main_ancestor()
     -> raises ProtocolError unless sha is reachable from FRESHLY FETCHED origin/main
```

**Bracket on the real `resolve_landed_sha`, inert local fixture** (bare origin
plus clone; no GitHub, and no obligation, label, approval or merge created
anywhere):

| case | count | result |
| --- | --- | --- |
| premise: both SHAs satisfy `SHA_RE` | 2/2 | the shape pre-filter genuinely cannot tell them apart — the divergence *does* exist at that layer |
| POSITIVE: genuine ancestor | 1/1 | returned `50b30dcc…` — the gate is not inert |
| NEGATIVE: same shape, **no ancestry** | 1/1 | REFUSED, "is not reachable from fetched origin/main" |
| CANNOT-RUN: missing source checkout | 1/1 | REFUSED **loudly**, not silent |

No bypass: all three arming sites (`:332`, `:442`, `:526`) route through
`arm_sha`. No leaked pre-arm evidence: the intent record does write
`landed_sha` before arming, but the intent directory has exactly one consumer —
`land_and_arm.py` itself, whose recovery path at `:487` arms through the same
gate. `merged-unarmed` appears nowhere else as evidence.

Shape is used where shape suffices; ancestry is checked where ancestry is
claimed.

**Why the audit got it wrong, which matters more than the row.** I traced the
claim to line 138 and stopped, instead of following the call chain to its gate.
That is the same substitution the audit exists to catch — I checked what the
code *said* rather than what it *does*. This document had already warned that a
call-site count cannot distinguish a gate from a log line; the retraction is
that warning applied to its own author. The method section's rule stands and
should be read as covering this case too: **a status claim rests on reading the
call site through to the decision, not on reading the predicate.**
