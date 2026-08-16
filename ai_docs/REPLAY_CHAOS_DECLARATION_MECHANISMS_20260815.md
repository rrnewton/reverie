# What blocks the replay and chaos declarations

Task: `what-blocks-the-replay-and-chaos-declarations`. Date: 2026-08-15. Host: devbig030.
Read-only diagnosis; nothing proposed, nothing edited.

> **Provenance.** Measured on devbig030 under the portable profile only — no `validate.sh`, no full and no privileged profile. **Nothing here is a receipt**, and nothing in it should be read as one now that a qualifying receipt on a red base binds its exact commit as hard green. devbig030 cannot mint full or privileged receipts: `arch_prctl(ARCH_SET_CPUID=0x1012, 0)` returns ENODEV here, re-measured 2026-08-15.


## Unit

Every figure below counts **declarations**: one entry in a mode's `backends_disabled` dict, i.e. one
`(test, mode, backend)` triple carrying a reason string. **Not cells, not reds.** 336 tests in the
manifests at hermit `main` `302a1a9c0` (equal to freshly fetched `origin/main`, tree clean).

Derived independently from `cargo run -p hermit-manifest-plan -- --format harness-json`, and
reconciled against the declared per-mode totals before use:
`replay 1679 | chaos 1678 | custom 1678 | verify 1091 | naked 303 = 6429` — an exact reproduction.
**This slice = replay 1679 + chaos 1678 = 3357 declarations.**

## Nothing consults the reason string

Complete consumer list for `backends_disabled`, from a grep across `ci/`, `scripts/`, `.github/`:

1. `ci/manifest-plan/src/main.rs:699-710` — validates that it is a table and that each entry has a
   non-empty reason. It never reads the content.
2. `ci/test_harness.sh:2341` `emit_gap_plan` — copies the string into a gap-plan row as `why`.

That row reaches exactly two places: `plan gaps` (reporting), and `run_required` under
`--probe-disabled` (`:4006`). `--probe-disabled` requires exact `--test` **and** `--mode` **and**
`--backend` simultaneously (`:2270-2272`), is `run`-only, and across all of CI is used for three
probes, all `verify`/`sabre`, non-gating (`.github/workflows/ci-portable.yml:848`).

**So 0 of the 3357 declarations are consulted by any gate in any normal harness run.** The reason
string is documentation. It is not inert in the strong sense — `--probe-disabled` can execute one,
named explicitly — but nothing reads it to decide anything.

## The dominant cause is that the mode is off, not that a backend is blocked

Cells are generated only from `backends_enabled`, and only when the mode has `ci = true`
(`ci/test_harness.sh:2300-2310`).

| mode | tests with `ci=false` **and** `backends_enabled=[]` | declarations under them |
| --- | --- | --- |
| replay | 335 of 336 | 1675 |
| chaos | 334 of 336 | 1670 |

**3345 of 3357 (99.6%) sit under a mode that runs for no backend on that test.** There is no running
ptrace sibling for the declaration to distinguish itself from. Only **12** declarations — replay 1
test × 4 backends, chaos 2 tests × 4 — sit beside something that actually runs.

At the mode level the cause is frequently unrecorded as well: **240 of the 335** replay tests and
**239 of the 334** chaos tests have no `ci_disabled_reason` at all. The key is optional in the schema
(`ci/manifest-plan/src/main.rs:653-657` lists it as allowed, not required).

## What would refuse each declaration if probed — measured, not read

### replay: a real CLI gate, and the reason strings are accurate

Harness shape `hermit --log=info --backend B record start --strict --verify --verify-json J
--data-dir D --record-timeout N -- guest` (`:3470-3473`), bracketed in both directions:

| backend | rc | first line |
| --- | --- | --- |
| ptrace | 0 | `:: Recording...` — **accepted**, which is what makes the gate a gate |
| dbt | 1 | `the DBT backend is available only through 'hermit --backend dbt run'; record and replay …` |
| kvm | 1 | `the KVM backend is available only through 'hermit --backend kvm run'; record and replay …` |
| sabre | 1 | `the SaBRe backend is available only through 'hermit --backend sabre run' and … strace` |
| liteinst | 1 | `the LiteInst preload backend is available only through 'hermit --backend liteinst run'` |

Source: `hermit-cli/src/bin/hermit/main.rs validate_backend_scope:241-281`. **1344 declarations**
(336 tests × 4 backends) are backed by this gate, and their reason strings — `Record/replay is
unsupported by X` — are accurate.

### chaos: the CLI gate reaches none of it

Harness shape `hermit --log=info run --base-env=minimal --backend B --strict --verify
--verify-allow=both --verify-json J --chaos --sched-heuristic=random --seed=S -- guest` (`:3480-3484`):
**all five backends exit 0** on `/bin/true`. Chaos is a `run`, and `validate_backend_scope` only
restricts non-`run` subcommands. **The CLI gate reaches 0 of the 1678 chaos declarations.**

The two modes needed opposite answers here, and any clustering done on the reason strings would have
merged them: `Record/replay is unsupported by KVM` and `Chaos scheduling is unsupported by KVM` are
the same shape and have nothing in common mechanically.

### chaos: the capability claim, tested on the one cell where it is testable

1320 chaos declarations assert `Chaos scheduling is unsupported by {KVM, LiteInst, SaBRe, DBT}` (plus
14 still spelled `DBI`, a stale backend name; the manifest key is `dbt`).
`determinism-stress/order-violation` is the single chaos cell that is on, its four disabled backends
carry exactly those strings, and its manifest declares seeds 0–31 with `min_distinct=2`,
`min_passes=1`, `min_failures=1`. All 32 seeds on all five backends — 160 runs — scored against the
manifest's own assertions plus the same-seed chaos verdict gate (`assert_chaos_verify_verdict`,
`:3337-3360`):

| backend | distinct | passes | oracle trips | timeouts | chaos verdict ok | verdict |
| --- | --- | --- | --- | --- | --- | --- |
| ptrace (control) | 2 | 28 | 4 | 0 | 32/32 | PASS |
| sabre | 3 | 22 | **4** | 6 | 26/32 | FAIL |
| kvm | 2 | 28 | 0 | 4 | 0/32 | FAIL |
| dbt | 2 | 28 | 0 | 4 | 0/32 | FAIL |
| liteinst | 1 | 0 | 0 | 0 | 0/32 | FAIL |

Grading the zeros rather than letting `distinct=2` speak:

- **SaBRe produces the chaos diversity.** Its 4 oracle trips carry stdout hash `a9169bc2fe0e04f6` —
  byte-identical to ptrace's violation class. It finds the same order violation ptrace finds. What
  fails it is 6 of 32 seeds timing out at the cell's 90 s bound. `Chaos scheduling is unsupported by
  SaBRe` misdescribes the block: the scheduling works, the run does not finish.
- **KVM and DBT never trip the oracle once in 32 seeds.** Their `distinct=2` is
  `{success, timeout}` — the second class is a harness kill with empty stdout, not a schedule
  outcome, so their real distinct schedule count is 1. Both also fail the verdict gate for
  verdict-*shape* reasons: kvm writes `verdict=matched` but omits `compare_logs`; dbt writes no
  verdict at all (`verdict=no_result`).
- **LiteInst fails all 32 seeds** with `Error: -524 ENOTSUPP (Operation is not supported)` — the
  pre-existing LiteInst -524 limit, not a chaos-scheduling property.

**Four mechanisms hide under one reason-string shape** — a timeout, an absent verdict, a malformed
verdict, and -524 — needing four different remedies, and none of them is "chaos scheduling is
unsupported".

## Mechanism clusters

| mechanism | declarations | of which the mode is ON |
| --- | --- | --- |
| replay / CLI gate refuses `record` | 1344 | 4 |
| chaos / dbt writes no verdict | 336 | 2 |
| chaos / kvm verdict omits `compare_logs` | 336 | 2 |
| chaos / liteinst -524 ENOTSUPP | 336 | 2 |
| chaos / sabre, 6 of 32 seeds time out | 336 | 2 |
| replay / ptrace, policy statement only | 335 | 0 |
| chaos / ptrace, mode simply off | 334 | 0 |
| **total** | **3357** | **12** |

The right-hand column matters more than the left. The left column is *what would refuse this if
someone probed it*, not *what is currently blocking a cell*.

## How much of this slice remains unexplained

Most of it.

1. **Measured directly on the declaration itself: 1348 of 3357 (40%)** — the 1344 replay CLI-gate
   declarations plus the 4 chaos declarations on `determinism-stress/order-violation`.
2. **Measured on a sibling and extended by inference: 1316.** The chaos per-backend mechanisms come
   from one fixture, one host, 32 seeds. 1320 chaos declarations carry the "unsupported" string
   across 336 tests; the claim is testable only where the mode is on, which is 1 test. This is a
   **floor** on how many reason strings misdescribe their mechanism, not a measurement of all of them.
3. **Cause partly established, reason absent: 2395** — declarations under the 240 replay / 239 chaos
   tests with no `ci_disabled_reason`. That the mode is off is established; *why* is not recoverable
   from the manifests.
4. **No mechanism found: 669** (replay/ptrace 335 + chaos/ptrace 334). The CLI accepts ptrace for
   both modes; the reasons are policy statements, which are decisions rather than blocks. Whether
   those cells would pass is untested.
5. **Not characterised at all:** the sabre 6/32, kvm 4/32 and dbt 4/32 timeouts. The deadline was not
   moved, so none can be called a hang. A timeout kill is not a duration.

## Open disagreements, carried rather than resolved

- **4750** is not reconstructible from these counts as a pool containing replay and chaos.
  `6429 − 1306 = 5123`; the only clean route to 4750 is `6429 − 1679`, which subtracts replay.
- **The CLI-gate exclusion does not match.** Mine is 1344 *declarations*, all in replay; the figure
  passed to me is at most 1306 *cells*. Different unit, different number. Nothing was subtracted —
  subtracting a figure that cannot be reproduced would be worse than overlapping, and the affected
  declarations are named above.
- **14 declarations per mode still spell the backend `DBI`** while the manifest key is `dbt`.

## Reproduction

`ignored/blocks-replay-chaos-20260815/` in the `dev-hermit` parent (gitignored: survives context
loss, not host loss). `harness.json` (manifest projection), `declarations.json` (all 3357),
`mechanisms.txt` (the cluster table), `chaosprobe/` (all 160 seed runs with per-seed verdict JSON).
