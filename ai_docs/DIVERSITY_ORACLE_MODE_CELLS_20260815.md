# What blocks the diversity-oracle mode cells

Task: `what-blocks-the-diversity-oracle-mode-cells`. Date: 2026-08-15. Host: devbig030.
Read-only diagnosis; nothing proposed, nothing edited.

## Unit and population

**Cells** (test × mode × backend as the manifest records them); naked cells carry backend `native`.
From the manifests at hermit `main` `302a1a9c0`, tree clean:

| mode | cells | enabled | disabled | scorecard status of the enabled ones |
| --- | --- | --- | --- | --- |
| naked | 336 | 33 | 303 | **all 33 red** |
| chaos | 1680 | 2 | 1678 | both green |

**`naked` is the only mode in the scorecard where every enabled cell is red** — verify has 167 green
of 589 enabled, chaos 2 of 2, replay 1 of 1. That asymmetry is what this class had to explain.

hermit-sabre's 40-cell sample was not available to me; this population was derived independently and
sabre's result is carried as a disagreement rather than adjudicated.

## The instrument was bracketed before any absence was read

| control | 3 runs | distinct | `min_distinct=2` would |
| --- | --- | --- | --- |
| constant-output guest | — | 1 | FAIL |
| varying-output guest | — | 3 | PASS |

The probe reports 1 when there is nothing to see. That leg is what makes a later `distinct=1`
readable as a result rather than as a null instrument.

## Mechanism 1 — no naked cell is ever selected (336 of 336)

All 336 have `modes.naked.ci = false`. The CI matrix is built from
`test_harness.sh plan --lane portable --ci-only` and the runner passes `--ci-only`
(`.github/workflows/ci-portable.yml:368`, `:753-755`); `--ci-only` forces
`select(.value.ci == true)`. Independently, `ci/expected-e2e-plan.json` — the 172-cell required plan
— holds `verify 167 | chaos 2 | custom 2 | replay 1` and **zero naked cells**.

Not a loophole, but worth recording: the selector *does* exempt naked from the `ci` gate
(`select(.value.ci == true or $mode_filter == "naked" …)`, `:2300`), so a human running
`--mode naked` without `--ci-only` would run them. CI never does.

## Mechanism 2 — fixing selection would not turn most of them green

The naked oracle was run directly, scored exactly as `ci/test_harness.sh:3681-3711` scores it
(observation = sha256 over status + stdout, `:3084-3109`; `E2E_TMPDIR` refreshed per attempt).

**The 33 enabled cells: 33 of 33 oracle-met**, every one at `distinct == runs`, zero non-zero exits.
Their red is purely non-selection; they would pass today.

That uniformity is the shape sabre's error took, so it was checked a second way rather than banked:
direct byte comparison of raw stdout, bypassing the hash. `openssl-rand` emits different 32-byte hex
per run; `producer-consumer` different thread interleavings; `python-hash-determinism` different dict
iteration orders; `proc-uptime` ticks `413183.75 → .76 → .77`. The population is also homogeneous by
design — these 33 are openssl / uuidgen / mktemp / shuf / ssh-keygen / date-nanoseconds / dev-random
and language RNG cells, chosen precisely because they vary.

**The 303 disabled cells.** 261 carry one bulk scope statement — *"This migration inventories strict
verification; it does not assert native nondeterminism"* — which records that nobody asserted
anything, not a block. The other 42 make falsifiable claims; 16 were tested at 5 native runs each.

- **Group A, prose claims the guest is deterministic natively — 8 of 8 confirmed `distinct=1`.** The
  oracle is genuinely unsatisfiable, which is not a defect in the cell. Enabling these would
  manufacture a red, not recover a green.
- **Group B, prose says native varies but is not guaranteed to within a bounded run** — mixed exactly
  as the prose predicts: `thread-output` 5/5 met; `order-violation` and `thread-interleaving` both
  `distinct=1`. The `thread-interleaving` result independently reproduces what li2 and kvm2 saw, and
  is why that guest was never a valid chaos probe.
- **Group C, prose says the native run differs every time and exists as the negative control** — 3 of
  5 accurate (`file-timestamp-identity`, `startup-surface-identity`, `stat-metadata-identity`, all
  5/5). These are satisfiable and disabled for a **role** reason, not a capability one.
  - **One refuted:** `system-utils/shm-coherency-identity` claims "the reader catches a different
    prefix of the writer's data each run" and measured `distinct=1/5`.
  - **One is a third mechanism:** `backend-parity-c/host-identity` exits 1 on all five runs —
    accurate to its own prose — and the gate fails on `failed_runs > 0` (`:3705`) *before*
    distinctness is consulted. It produced 3 distinct outputs and still can never pass.
    Non-zero-exit-by-design and unsatisfiable-oracle are different mechanisms that any clustering on
    `distinct` alone would merge.

## Mechanism summary

| mechanism | cells |
| --- | --- |
| never selected (whole mode) | 336 of 336 naked |
| would pass if selected (enabled subset, measured) | 33 of 33 |
| oracle unsatisfiable, `distinct=1` (probed subset) | 10 of 16 |
| oracle satisfiable, disabled for a role reason | 3 of 16 |
| guest exits non-zero natively by design | 1 of 16 |
| prose refuted by measurement | 1 of 16 |

The 10 / 3 / 1 partition the 16; `shm-coherency-identity` is one of the 10 and is also the refuted one.

## How much remains unexplained

**Measured: 49 of 336 naked cells (14.6%)** — the 33 enabled plus 16 probed disabled.
**Not measured: 287**, of which 261 carry the bulk scope string and 26 are substantive reasons not
probed (5 are non-program `direct` cells the driver cannot run; 21 were simply not reached).

The 10-of-16 rate is **not** extrapolated onto the 261: that string says only that nobody asserted
native nondeterminism for the fixture, which predicts nothing about whether it varies.

The chaos side was established in `what-blocks-the-replay-and-chaos-declarations` and is not redone
here — 2 enabled cells green, 1678 disabled reached by zero CLI gate, four distinct mechanisms
measured on the single chaos cell that is on. That remains 1 test of 336.

## Disagreement carried, not resolved

sabre reports that the diversity-oracle modes refute plan membership. This naked measurement points
the other way: non-selection explains all 336, and for the 33 enabled it is the only thing wrong with
them. sabre's sample was not available, and it could have drawn from the enabled 33, the disabled 303
or the chaos 1678 — three populations that give three different answers. Both readings are recorded.

## The two readings agree on the part that matters

**Plan membership explains why these cells do not run. It does not predict that they would pass.**
For the 33 enabled naked cells it does — 33 of 33. For the disabled cells probed here, 10 of 16 would
still be red after selection was fixed, because the oracle demands a diversity the guest cannot
produce. Pass-if-selected and qualified are different claims, and for this class they come apart.

## Per-cell results

### The 33 enabled naked cells

| cell | runs | distinct | oracle |
| --- | --- | --- | --- |
| `data-handling/jq-json-transform` | 3 | 3 | ORACLE-MET |
| `data-handling/sqlite-query-determinism` | 3 | 3 | ORACLE-MET |
| `determinism-stress-c/producer-consumer` | 5 | 5 | ORACLE-MET |
| `determinism-stress/thread-contention` | 5 | 5 | ORACLE-MET |
| `language-runtimes/bash-random` | 3 | 3 | ORACLE-MET |
| `language-runtimes/cpp-stl-determinism` | 3 | 3 | ORACLE-MET |
| `language-runtimes/gawk-random` | 3 | 3 | ORACLE-MET |
| `language-runtimes/lua-random` | 3 | 3 | ORACLE-MET |
| `language-runtimes/m4-macro-mkstemp` | 3 | 3 | ORACLE-MET |
| `language-runtimes/node-v8-jit` | 3 | 3 | ORACLE-MET |
| `language-runtimes/perl-hash-order` | 3 | 3 | ORACLE-MET |
| `language-runtimes/perl-random` | 3 | 3 | ORACLE-MET |
| `language-runtimes/python-hash-determinism` | 3 | 3 | ORACLE-MET |
| `language-runtimes/python-hashseed` | 3 | 3 | ORACLE-MET |
| `language-runtimes/python-random` | 3 | 3 | ORACLE-MET |
| `language-runtimes/ruby-random` | 3 | 3 | ORACLE-MET |
| `language-runtimes/rust-hashmap-iteration` | 3 | 3 | ORACLE-MET |
| `language-runtimes/tcl-rand-clock` | 3 | 3 | ORACLE-MET |
| `system-utils/date-nanoseconds` | 3 | 3 | ORACLE-MET |
| `system-utils/mcookie-random` | 3 | 3 | ORACLE-MET |
| `system-utils/mktemp-name` | 3 | 3 | ORACLE-MET |
| `system-utils/openssl-enc` | 3 | 3 | ORACLE-MET |
| `system-utils/openssl-genpkey` | 3 | 3 | ORACLE-MET |
| `system-utils/openssl-passwd` | 3 | 3 | ORACLE-MET |
| `system-utils/openssl-rand` | 3 | 3 | ORACLE-MET |
| `system-utils/openssl-x509` | 3 | 3 | ORACLE-MET |
| `system-utils/proc-random-uuid` | 3 | 3 | ORACLE-MET |
| `system-utils/proc-uptime` | 3 | 3 | ORACLE-MET |
| `system-utils/random-device` | 3 | 3 | ORACLE-MET |
| `system-utils/shuf-permutation` | 3 | 3 | ORACLE-MET |
| `system-utils/sort-random` | 3 | 3 | ORACLE-MET |
| `system-utils/ssh-keygen-ed25519` | 3 | 3 | ORACLE-MET |
| `system-utils/uuidgen-random` | 3 | 3 | ORACLE-MET |

### The 16 probed disabled naked cells (5 native runs each)

| group | cell | distinct/5 | oracle |
| --- | --- | --- | --- |
| A-claims-deterministic | `applications/c-toolchain-workflow` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `applications/git-repository-workflow` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `data-handling/archive-roundtrip` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `data-handling/shell-pipeline` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `data-handling/zstd-multithread` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `backend-parity-c/event-delivery-ordering` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `backend-parity-c/mmap-layout-pointer-order` | 1 | UNSATISFIED(distinct=1) |
| A-claims-deterministic | `backend-parity-c/short-io-split-identity` | 1 | UNSATISFIED(distinct=1) |
| B-varies-but-not-guaranteed | `determinism-stress/thread-output` | 5 | MET |
| B-varies-but-not-guaranteed | `determinism-stress/order-violation` | 1 | UNSATISFIED(distinct=1) |
| B-varies-but-not-guaranteed | `determinism-stress/thread-interleaving` | 1 | UNSATISFIED(distinct=1) |
| C-claims-differs-every-run | `system-utils/file-timestamp-identity` | 5 | MET |
| C-claims-differs-every-run | `system-utils/startup-surface-identity` | 5 | MET |
| C-claims-differs-every-run | `system-utils/shm-coherency-identity` | 1 | UNSATISFIED(distinct=1) |
| C-claims-differs-every-run | `backend-parity-c/stat-metadata-identity` | 5 | MET |
| C-claims-differs-every-run | `backend-parity-c/host-identity` | 3 | NONZERO-EXIT |

## Reproduction

`ignored/diversity-oracle-20260815/` in the `dev-hermit` parent (gitignored: survives context loss,
not host loss). `naked33.json` / `naked33.results.json` (the enabled population and its oracle
results), `groups.json` / `probe.results.json` (the probed disabled cells), `naked.sh` (the driver,
with the harness line numbers it mirrors), `runs/<cell>/` (per-attempt stdout and hashes).
