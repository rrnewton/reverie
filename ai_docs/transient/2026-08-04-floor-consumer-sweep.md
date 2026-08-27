# Floor consumer sweep — who embeds or assumes a rebase-base floor

Task: `prs-predating-commit-anchoring-can-never-produce-a-qualifying-receipt`
Author: hermit-ghdag (opus-4.8), 2026-08-04
Companion: the floor *enumeration* + queryable tool landed as parent `f35a61d`
(`ci-hub/validate/gate_floors.py` + `rebase-base-floors.json`); see
[[merge-gate-v2-floor-invalidates-pre-floor-greens]],
[[rebase-base-floors-queryable-gate-floors-py]].

## The search key

A **floor** is asserted by any consumer that **fails closed on a missing/null field**
— the field did not exist before some commit, so a head predating that commit can
never satisfy the consumer, no matter how green its run. A consumer need not name a
SHA to assert a floor; a fail-close on `commit_anchored` / `executed_tests` **is** a
floor at the commit that first emitted that field. This sweep enumerates every such
consumer in `ci-hub/` and classifies it by whether it **reads the registry** (floor
explicit, one authority) or **re-encodes the floor inline** (the covered-path defect).

## Class 1 — the rebase-base / receipt floor (drain-blocking)

### 1a. REGISTRY READERS — covered path, floor explicit and queryable

| # | Consumer | Site | How it reads the floor |
|---|----------|------|------------------------|
| 1 | `gate_floors.py` | `validate/gate_floors.py` | THE authority: derives effective floor from `rebase-base-floors.json` + git first-parent |
| 2 | `preflight_anchor.py` | `validate/preflight_anchor.py` | reads `rebase-base-floors.json`; refuses a head predating any floor before validate |
| 3 | drain prevalidate | `landing/parallel-prevalidate.sh:78` | calls `preflight_anchor.py` in `run_one`; exit 2 → skip |
| 4 | `ci-hub newest-green` | `ci-hub.rs:3371 query_effective_gate_floor`, `:3604` | shells `gate_floors.py --json`; **no Rust floor const** (test `effective_floor_follows_registry_output_without_a_rust_constant`). NOTE: this is hermit-ci's **uncommitted** working-tree edit — the earlier hardcoded `CURRENT_GATE_SCHEMA_FLOOR` const is already gone here |

### 1b. INLINE FAIL-CLOSED CERTIFIERS — bypass path, floor re-encoded per site

Each independently re-encodes "a qualifying receipt carries fields X,Y,Z" — the DUAL
of the floor (a pre-floor head emits them NULL) — without consulting the registry.
Five sites, three languages:

| # | Consumer | Site | Role | Field gate |
|---|----------|------|------|-----------|
| 5 | `is_clean_full_pass` | `lib/validate_status.rs:215` | landing/cache certifier (the one the task cites) | commit_anchored, tree_dirty=false, selection_mode/profile=full, result=pass, executed>0, coverage@schema≥5. **NO filtered_tests guard** ("Filtered no longer gates") |
| 6 | `_row_full_pass` | `history/query.py:925` | green-time ledger corroborator | commit_anchored, tree_dirty=false, selection_mode/profile=full, result=pass, executed∉{None,0}, **filtered_tests==0**. NO coverage, NO schema-version awareness |
| 7 | `qualifying_row` | `validation/publish_receipt.py:34` | merge-gate receipt PUBLISHER | + result=pass, **failures==0**, executed>0, coverage@schema≥5. NO filtered guard |
| 8 | receipt gate | `validation/verify_receipt.sh:92` | merge-gate receipt VERIFIER (runs from immutable parent) | jq: profile/selection=full, anchored, !dirty, pass, **failures==0**, executed>0, coverage@`schema<5` grandfather. Hardcodes `5` inline |
| 9 | anchor filters | `lib/history_queries.rs:211,272` | trustworthy-recorded counter + first_bad | inline `commit_anchored && tree_dirty` (partial: anchor fields only) |

## Class 2 — merge-gate floor authority OUTSIDE ci-hub

The merge-gate-v2 floor (`c369be3f`) is enforced by **GitHub branch protection +
`merge-gate.yml`** in the hermit repo — `git grep` for the SHA / `gate-schema` in
`hermit/.github` is empty, confirming the floor is workflow/branch-protection config,
not in-repo code. It **cannot read the registry**; `rebase-base-floors.json` mirrors it
as a `kind=merge-gate` entry that must be kept in sync by hand. See
`ai_docs/2026-08-04-merge-gate-branch-yaml-authority-audit.md`.

## Class 3 — same signature, DIFFERENT floor artifact (not drain-blocking)

Fail-closed on `schema_version` for their own ledger/envelope/event/intent — a floor
on a different artifact, enumerated for completeness so they are not conflated with the
receipt floor:

- `ci-hub.rs:4182` (`record.schema_version != 1`)
- `directives/check.py:147` ("ledger schema_version must be 1")
- `health/operational_health.py:472` (health envelope schema)

## The covered-path defect, made concrete — FOUR live drifts among 1b

The registry made the floor-as-a-COMMIT queryable and 4 paths read it. But the
receipt-field predicate (the floor's dual) is duplicated across the 5 certifiers with
**no shared source**, and has already diverged:

- **DRIFT-1 (filtered_tests):** `query.py` requires `filtered_tests==0`;
  `validate_status.rs`, `publish_receipt.py`, `verify_receipt.sh` do **not**
  (validate_status.rs explicitly dropped it). ⇒ a legit-filtered receipt
  (executed>0, filtered>0) is **green for landing but not green for green-time**.
- **DRIFT-2 (COUNTS_SCHEMA):** the `5` threshold is redefined 3× —
  `validate_status.rs:167`, `publish_receipt.py:25`, `verify_receipt.sh` inline `<5`.
  `query.py` has **no schema-version awareness at all**, so it treats a count-less
  schema-3 row like a schema-5 row. Move the threshold to 6 and 3 files change while
  query.py silently stays wrong.
- **DRIFT-3 (failures):** `publish_receipt.py` + `verify_receipt.sh` require
  `failures==0`; `validate_status.rs` + `query.py` don't check it (key on
  `result=="pass"`). Two definitions of "no failures."
- **DRIFT-4 (coverage):** `validate_status.rs`/`publish_receipt.py`/`verify_receipt.sh`
  enforce per-node coverage on schema≥5; `query.py` has **no coverage clause**.

## Recommendation (the fix — a follow-on task, not this sweep)

One shared **qualifying-receipt predicate** — required-field set + `COUNTS_SCHEMA` +
filtered/failures/coverage rules — that all five 1b certifiers consume, colocated with
or referenced by the registry, so a schema tightening is a **single edit** instead of
"registry `_pending`→`anchors` + 5 code sites in 3 languages." Cross-repo/lang means it
cannot be one function; the minimum viable version is a single JSON/const the Rust, the
Python, and the jq gate all read. Owners to coordinate: `validate_status.rs` +
`history_queries.rs` = hermit-ci (currently **uncommitted** — do not edit that tree);
`query.py` = green-time; `publish_receipt.py` + `verify_receipt.sh` = landing/merge-gate;
the counts fields = hermit-243 (`_pending` counts anchor still `sha:TBD`).

## Verification BY MUTATION (the strong proof — perturb the registry, watch each answer)

Static grep *classifies*; mutation *proves*. The registry's effective floor was
swapped in a **copy** (live `rebase-base-floors.json` never touched — both readers
take a path override: `gate_floors.py --registry`, `preflight_anchor.py --anchors`)
from the real floor `c369be3f` (hermit first-parent idx 2) to `b4e94ce4` (idx 0,
the current tip — the same SHA hermit-sabre is rebasing the 12 low-band heads onto).
Discriminator head for the preflight: `1b12bc1a` (idx 1) — contains `c369be3f`,
does NOT contain `b4e94ce4`.

| Consumer | LIVE registry | MUTATED registry | Moved? |
|----------|---------------|------------------|--------|
| `gate_floors.py` `effective_floor` | `c369be3f` | `b4e94ce4` | **YES — reads it** |
| `preflight_anchor.py --head 1b12bc1a` | OK / exit 0 | REFUSE / exit 2 (names `b4e94ce4`) | **YES — reads it** |
| `ci-hub newest-green` (Rust) | (transitive) | (transitive) | **YES, transitively** — shells `gate_floors.py`, parses `effective_floor`; no independent registry path (guarded by test `effective_floor_follows_registry_output_without_a_rust_constant`). Not independently mutated because the binary hardcodes the registry path — which is the correct single-authority design |
| `validate_status.rs::is_clean_full_pass` | — | — | **NO — cannot move** |
| `history/query.py::_row_full_pass` | — | — | **NO — cannot move** |
| `validation/publish_receipt.py::qualifying_row` | — | — | **NO — cannot move** |
| `validation/verify_receipt.sh` | — | — | **NO — cannot move** |
| `lib/history_queries.rs` anchor filters | — | — | **NO — cannot move** |

The 5 "cannot move" verdicts are not asserted — they are **structural**: each of the
five files has **0** references to `rebase-base-floors|gate_floors|effective_floor|
preflight_anchor` (grepped). Their verdict is a pure function of the receipt row's
fields; the floor SHA is never an input, so no registry mutation can reach them.
**That is the covered-path defect, demonstrated by perturbation: 3 consumers move
with the registry, 5 are blind to it** — and the 5 are exactly the ones re-encoding
the floor's dual (the required-field set) inline, which is why they have already
drifted 4 ways (above). Reproduce: `sed s/c369be3f…/b4e94ce4…/ registry >copy` then
run each reader `--registry`/`--anchors copy` and grep the 5 certifiers for registry
refs.

## Verification of this sweep

`git grep` in `ci-hub/` for: floor SHAs (`c369be3f|bfb0a9ef|e8a0d8d3|525627be|4cdda392`);
floor identifiers (`gate_schema_floor|effective_floor|rebase-base-floor`); fail-close
fields (`commit_anchored|selection_mode|tree_dirty|executed_tests|filtered_tests|
schema_version|is_clean_full_pass|full_pass`); registry readers (`rebase-base-floors|
gate_floors|preflight_anchor`). Non-test, non-`.md` hits triaged into the classes above.
Registry-reader set = {gate_floors.py, preflight_anchor.py, parallel-prevalidate.sh,
ci-hub.rs newest-green}. Inline-certifier set = the five in 1b.
