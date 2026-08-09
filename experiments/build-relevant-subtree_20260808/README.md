# Build-Relevant Subtree (BRS) for receipt reuse — proposal, evidence, brackets

**Status:** proposal for review by `cihub-auditor` (owner of the receipt predicates).
Nothing here is landed. Design half **D** of the atomicity fix; half **A** and the
`producer` provenance field are already landed.

**Author:** `[impl agent, claude-opus-5] [reverie-pin-devbig014]`, 2026-08-08.

---

## 1. The question

A Hermit receipt was validated against Reverie pin `X`. Reverie main moves to `Y`.
May the receipt be **reused** for `Y`, or must Hermit revalidate (~12 min)?

Answer proposed here: reuse iff `BRS(X) == BRS(Y)`, where BRS digests exactly the
Reverie content that can enter a Hermit build.

## 2. Headline finding — read this before anything else

**The payoff is 4%, not "most". My own earlier prediction was wrong and the
measurement refutes it.**

When I deferred this decision I wrote that "most reverie commits that stale a hermit
pin are exactly this kind [build-irrelevant]". Measured over **48 real historical pin
bumps** reconstructed from `hermit/Cargo.lock` history: **2 of 48 (4%)** would have
been reusable. Not most.

The error was a selection effect: the bump I happened to be executing
(`038e9939 → 108f9ab4`, docs-only) is one of the two. Generalising from the case you
are standing in is exactly the trap, and I fell into it.

Reuse rate collapses with how far the pin jumps, which is why per-commit intuition
misleads — pin bumps skip several commits at once:

| gap (reverie commits skipped) | pairs | REUSE | rate |
| --- | --- | --- | --- |
| 1 | 150 | 38 | 25.3% |
| 2 | 149 | 14 | 9.4% |
| 3 | 148 | 5 | 3.4% |
| 5 | 146 | 0 | 0.0% |
| 10 | 141 | 0 | 0.0% |

So D is worth having **only for short gaps** — which is precisely the window half **A**
creates. A alone shrinks exposure from hours to the merge transaction; D then makes
~25% of the *remaining* gap-1 races free. D without A is worth ~4%.

**Recommendation:** implement D, but bill it as a race-window optimisation that
composes with A, not as a general 12-minutes-to-2-seconds win. It is cheap and never
wrong when fail-closed; it is not a major lever on its own.

## 3. The definition

```
BRS(rev) = sha256 over sorted (blob_sha, path) for every tracked path at rev that is
           either (a) inside a package directory cargo resolves into the CONSUMER's
           dependency graph, or (b) a workspace-level resolution file.
Reuse permitted iff BRS(X) == BRS(Y), computed over the UNION of the package sets
at X and Y.
```

Package dirs are read from `cargo metadata` on the consumer, never hand-listed, so the
set tracks the dependency graph automatically. Measured at Hermit `f65f7446`: **17
packages**, and note **five live under `experimental/`**, which also contains
non-consumed content — so any directory-prefix rule is wrong; the set must be
path-exact.

Relevant set size: **1473 of 2453 tracked files (60%)** at `108f9ab4`.

### Why deny-list, not allow-list — the load-bearing design choice

- **Allow-list** the relevant paths → a NEW unclassified path is treated as irrelevant
  → **fails open** → a receipt certifies a build it never validated.
- **Deny-list** the provably-inert paths → a new unclassified path is relevant
  → **fails closed** → worst case is one needless 12-minute revalidation.

False reuse is a silent wrong green; false refusal costs 12 minutes. The asymmetry is
not close. Everything below is constrained to fail closed.

### Guards (fail-closed preconditions, not niceties)

Crate-dir scoping is only sound while these hold. They are **accidents of today**, so
they must be enforced, or the definition silently rots into fail-open:

- **G1** no consumed build script reads outside its own package directory.
  *Verified:* the only two consumed `build.rs` (`reverie-dbi`, `experimental/reverie-sabre`)
  resolve every path from `CARGO_MANIFEST_DIR` and declare it via `cargo:rerun-if-changed`.
- **G2** no consumed lib/bin target `include_str!`/`include_bytes!` outside its package.
- **G3** no submodule gitlink inside a consumed package directory.
- **G4** no consumed build script references `third-party/` (see §5).

`brs.py` implements G3 and **refuses with exit 2** rather than emitting a hash it
cannot stand behind. G1/G2/G4 are currently verified by inspection and must be
mechanised by the implementer.

## 4. Brackets — counts both ways

### Negative (the half that matters): 9/9 planted build-changing deltas REFUSED

Planted on an inert local clone (`/tmp/brsfix`), never the live tree, never an artifact
that is itself an authorization. Every case must read `REVALIDATE-REQUIRED`:

| # | planted delta | strict | no-tests |
| --- | --- | --- | --- |
| N2 | edit consumed lib source (`reverie/src/tool.rs`) | REVALIDATE | REVALIDATE |
| N3 | edit vendored native source (dynamorio `CMakeLists.txt`) | REVALIDATE | REVALIDATE |
| N4 | edit consumed crate `Cargo.toml` | REVALIDATE | REVALIDATE |
| N5 | edit workspace `Cargo.toml` | REVALIDATE | REVALIDATE |
| N6 | edit a consumed `build.rs` | REVALIDATE | REVALIDATE |
| N7 | **add** a new `.rs` to a consumed crate | REVALIDATE | REVALIDATE |
| N8 | **delete** a file from a consumed crate | REVALIDATE | REVALIDATE |
| N9 | edit **only** the vendored `REVISION` marker | REVALIDATE | REVALIDATE |
| N10 | edit a `README.md` **inside** a consumed crate | REVALIDATE | REVALIDATE |

Plus a **real historical** negative: `6144323c → 038e9939`, which touched
`reverie-dbi/native/client.c` → REVALIDATE in both modes.

**Guard fires, not inert:** planting a submodule gitlink at `reverie/vendored-sub`
produced `REFUSE: guard violation … G3 submodule gitlink inside a consumed package`,
**exit code 2** — not a hash.

### Positive: 6/6 provably-inert deltas permitted (gate is not vacuously refusing)

| # | planted delta | verdict |
| --- | --- | --- |
| P1 | `AGENTS.md` | REUSE |
| P2 | `docs/` | REUSE |
| P3 | reverie's own `validate.sh` | REUSE |
| P4 | `reverie-examples/` (not consumed) | REUSE |
| P5 | top-level `tests/` (not consumed) | REUSE |
| P6 | `.github/workflows/ci.yml` | REUSE — **see §5** |

And the real proof case: `038e9939 → 108f9ab4` → `BRS` identical over 1473 files,
`REUSE-PERMITTED`.

## 5. The arguable boundaries — presented, with measured costs

### 5.1 `tests/` and `benches/` inside a consumed package — RESOLVED BY MEASUREMENT

This is the boundary I refused to decide alone. It turns out **not to matter**:

- `no-tests` changes the relevant set by 48 files (1473 → 1425).
- Over 48 real pin bumps it buys **0 additional reuses** (2/48 under both modes).
- Over 150 gap-1 deltas it buys **3** (38 → 41, +2 pp).

Its soundness rests on a premise — that the consumer compiles only lib/bin targets of a
git dependency, never its integration tests. That premise is cargo's documented
behaviour, but it *is* a premise.

**Recommendation: take `strict`. Drop `no-tests` entirely.** Paying any premise risk
for 0 additional real-world reuses is a bad trade. The argument dissolves rather than
needing to be won.

### 5.2 The `.github/` trap — why "obviously inert by path" is unsound in general

Measured in-tree: `reverie-dbi/tests/build_script_source.rs:66` does
`include_str!("../../.github/workflows/ci.yml")`. A file under `.github/` — which every
instinct calls CI-only and inert — is compiled into a Rust target.

It does **not** affect Hermit, because that is an integration test of a git dependency
and cargo never builds it. So P6's `REUSE` verdict is correct *for this consumer*. But
it is a concrete, in-repo demonstration that the definition must be scoped to **what
the consumer compiles**, not to what a path looks like. Guard **G2** is what keeps this
from becoming a real hole.

### 5.3 `.gitmodules` — a genuine trade-off, stated not silently picked

`third-party/{dynamorio,sabre,e9patch}` are **submodule gitlinks**. No consumed build
script reads them today — both consumed build scripts use their own vendored copies
(`reverie-dbi/vendor/dynamorio`, which is a **real tree**, not a gitlink). So
`.gitmodules` is inert for Hermit and I exclude it.

- **Exclude** (proposed): keeps both reusable bumps.
- **Include** (more conservative): costs **1 of the 2** reusable bumps — **50% of the
  entire payoff** — to guard a hazard that is not currently live.

**Recommendation: exclude, and enforce G4** (no consumed build script references
`third-party/`). If G4 ever fails, `.gitmodules` and the gitlinks become relevant and
this decision must be revisited. Reviewer may reasonably overrule toward including it;
the cost is now quantified rather than argued.

### 5.4 Conservative inclusions that are free

Measured sole-cause-of-refusal counts across the 48 real bumps:

| included path | appears in refusing bumps | **sole** cause |
| --- | --- | --- |
| `Cargo.lock` (reverie's own) | 0 | **0** |
| `rust-toolchain.toml` | 1 | **0** |
| `Cargo.toml` (workspace) | 2 | **0** |

Reverie's own `Cargo.lock` and `rust-toolchain.toml` are arguably ignored by cargo for a
git dependency. Including them anyway costs **zero** reuse, so keep them: free
conservatism, one less premise to defend.

## 6. What the implementer still owns

1. Mechanise **G1, G2, G4** (only G3 is implemented here).
2. Decide where BRS lives in the receipt schema and bump the schema version; the
   `producer` field landed separately and must not be conflated with this.
3. Decide the consumer: which caller is permitted to act on `REUSE-PERMITTED`, and
   ensure exactly one verifier owns it (one authority, called by every consumer).
4. Re-derive the package set from `cargo metadata` at use time. **A stale hardcoded
   package list is the fail-open hole** — `brs.py`'s `DEFAULT_PKG_DIRS` is a recorded
   measurement for standalone runs, not a production source of truth.

## 7. Reproduce

```bash
R=~/work/dev-hermit/reverie
python3 brs.py --repo $R --compare 038e993926e45514264d30367b70df9b6ac3b9b8 \
                                   108f9ab47605a7a2e8ae40353fde21f8a8b2310c   # REUSE, rc=0
python3 brs.py --repo $R --compare 6144323c5dab8b521278fce206f8774360c2b05f \
                                   038e993926e45514264d30367b70df9b6ac3b9b8   # REVALIDATE, rc=1
python3 brs.py --repo $R --scan <A>..<B> --mode strict > scan.csv
```

`results.csv` holds the 48 real pin-bump verdicts; `scan-strict.csv` / `scan-no-tests.csv`
hold the 150-commit gap-1 scans.
