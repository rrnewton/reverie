# Pin-gate refusal proof — independent verification

**Why this exists.** The ancestry+monotonicity change **weakens a gate**. Every other change today
was verified by the gates staying strict; this one cannot be, because it *is* the gate. If the
brackets are wrong the failure mode is **silent permanent acceptance** — a regressed pin lands and
nobody knows why, weeks later. The author of the change also wrote its tests, which is the weakest
link, so this artifact exists to be re-run by someone else.

**Run it:**
```
./prove-refusals.sh <hermit-checkout-with-the-change>
```
Exit 0 iff all four cases produce the required verdict. Needs network (`with-proxy`).

Shares **nothing** with the author's unit suite: real `rrnewton/reverie` remote, real Reverie SHAs,
the real compiled checker through its real launcher, inert temp-git fixtures. No stubs, no fixture
remote, no test harness.

## Result (2026-08-08, checker at hermit `265e84d4`)

| case | verdict | required |
| --- | --- | --- |
| lagging pin | rc=0 accepted | accept |
| equal to base | rc=0 accepted | accept |
| **REGRESSED pin** | **rc=1 REVERIE PIN REGRESSION - BLOCKED** | **refuse** |
| **OFF-HISTORY pin** | **rc=1 NOT ON reverie/main HISTORY - BLOCKED** | **refuse** |

4 passed, 0 failed.

## The refusals have been SEEN TO FAIL — a bracket that never fails is not evidence

Each clause was deliberately broken, the proof re-run, and the clause restored.

**Mutation 1 — disable monotonicity** (`base_pin(...).filter(|_| false)`):

```
[FAIL] REGRESSED pin    rc=0 want=1   Reverie pin is on main history and does not regress: 038e993
[ok]   OFF-HISTORY pin  rc=1          NOT ON reverie/main HISTORY - BLOCKED
```
The regressed pin is **accepted**, and the checker prints *"does not regress"* while doing it. That is
the silent-acceptance failure mode exactly. Off-history still refuses, so the two clauses are
independent rather than one check wearing two hats.

**Mutation 2 — reachability → object presence** (`cat-file -e` instead of `merge-base --is-ancestor`):

```
[FAIL] REGRESSED pin    rc=0 want=1   ...does not regress: 038e993
[FAIL] OFF-HISTORY pin  rc=0 want=1   ...does not regress: 88363a5
```
**Both** refusals stop firing. This is the tempting wrong predicate: `88363a56` is object-PRESENT in a
blobless fetch of `main` even though `main` has never contained it, so a presence test accepts a dead
commit. Only reachability refuses it.

**Restored:** 4 passed, 0 failed; working tree clean at `265e84d4`.

## The off-history fixture is real, not synthetic

`88363a56870dcdd88b13b1309233706b691f6a41` is a genuine dead commit: Hermit's own DBI→DBT rename half
pinned it, it lived only on `rename/dbi-to-dbt` (PR #390), and it was rebased away before merge — so
`rrnewton/reverie:main` has never contained it. A synthetic all-`f` SHA would have been a weaker test,
because it is *absent* rather than *present-but-unreachable*, and absence is the easy case.

## Substrate note — stated plainly

String equality was sufficient for the old rule; **a graph predicate is not the same shape**. Ancestry
and monotonicity are reachability questions, and `git ls-remote` returns a tip and nothing else. Any
harness or fixture that fakes the authority by stubbing `ls-remote` alone is **insufficient** for this
predicate and will produce misleading results — it must supply a real commit graph. That is why this
proof uses the real remote rather than approximating one, and why the in-repo harness fixture had to
be upgraded from a tip stub to a real local Reverie history.
