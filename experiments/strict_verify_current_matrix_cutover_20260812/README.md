# Canonical-comparator cutover on the current compatibility matrix

This artifact records the owner-directed before/after boundary for replacing
legacy `--verify` with `--verify-strict`. It deliberately does not reuse the
1,200-row legacy CSV or the 1,284-cell archived campaign: neither population
has exact-identifier overlap with the live ratcheted matrix.

## Before: legacy comparator

- Measured Hermit: `c6c6d6958a8dc940419ead146c8283f06607a9df`
- Measured Reverie: `c261050cfd41bec67e31bfd0cf6f56be008d0ebb`
- Host: `devbig014`
- Receipt: `devbig014-1786522425-2681754`
- Receipt digest: `10f4bbff88637ef51632b6d7f8d5a1d0eb1f525d856837d384b0e19d43cba79e`
- Receipt result: 58/58 nodes, 1,038/1,401 counted tests executed,
  363 filtered, zero failures, zero skipped nodes, clean commit-anchored tree.
- Declared population: 627 cells.
- Required-plan population: 172 cells.
- GREEN: **172/172** required-plan cells, evidenced at bucket granularity.
- STABLE FAIL: **0/172**.
- UNSTABLE: **0/172**.
- NO VERDICT: **0/172**.

The 627 declared cells are 337 ptrace, 141 SaBRe, 62 DBT, 31 LiteInst,
23 KVM, and 33 native bare-host controls. Native is not a backend. The 172
selected cells are 151 ptrace, 9 SaBRe, 9 DBT, and 3 LiteInst. KVM contributes
0 selected cells: its 2 `ci=true` cells are occasional and were not selected.
The remaining 453 declarations set `ci=false` with written reasons.

The selected mode split is 167 verify, 2 chaos, 2 custom, and 1 replay. At
this boundary, 166/167 selected verify cells use the legacy normalized
comparator; one explicitly opts into the canonical comparator. The receipt
does not retain per-cell outcomes, so 172/172 is a bucket-granular conclusion:
the exact 172-cell plan was ratcheted, every covering bucket ran non-vacuously,
and all covering nodes passed. It is not fabricated per-cell evidence.

## After: canonical comparator

- Measured Hermit: `9b5dfe93804d89f58eb5e2634de505e050001c8d`
- Measured Reverie: `c261050cfd41bec67e31bfd0cf6f56be008d0ebb`
- Host: `devbig014`
- Full-run identity: `validate-codex-tickgate-9b5dfe93804d-1786528397`
- Full log SHA-256: `2618e020aee2931e0a75c518f857c2b7576ba7f0e6c56b81b0a2ea761c673f8d`
- Safe-runner journal SHA-256: `45a422e52705262c2a84279ced903bdb6cd58a4bff3a64a5464f8ef1760674c9`
- Declared population: 627 cells.
- Required-plan population: 172 cells, with the same identity-set SHA-256 as
  the before side: `b3bab743a8c6e1fec6113c4f124874041b94a6414aa0e9edfb96910edfdde5ef`.

The direct observations were 150 canonical matches, 7 canonical divergences,
9 commands that produced no canonical comparison result, and 6 cells not run
after their bucket was cut from the keep-going DAG. A second exact-head
observation of SaBRe `system-utils/record-getpid` diverged; the full run and
five boxed exact-head reruns matched. Applying the established repeat-sweep
definitions produces this complete four-way partition:

| backend | GREEN | STABLE FAIL | UNSTABLE | NO VERDICT | total |
| --- | ---: | ---: | ---: | ---: | ---: |
| ptrace | 144 | 0 | 0 | 7 | 151 |
| SaBRe | 2 | 0 | 1 | 6 | 9 |
| DBT | 0 | 0 | 0 | 9 | 9 |
| LiteInst | 3 | 0 | 0 | 0 | 3 |
| **total** | **149** | **0** | **1** | **22** | **172** |

`NO VERDICT` includes seven one-observation canonical divergences: without a
repeat they prove neither a consistent failure nor a flake. It also includes
the nine DBT cells whose commands exited without a canonical verdict and the
six unexecuted ptrace cells. This deliberately does not improve the number by
calling absent evidence red or green.

The seven observed canonical divergences were:

- ptrace `language-runtimes/node-v8-jit`;
- SaBRe `c-programs/add-key-enosys`;
- SaBRe `c-programs/cachestat-enosys`;
- SaBRe `c-programs/futex-waitv-enosys`;
- SaBRe `c-programs/get-robust-list-self`;
- SaBRe `c-programs/kcmp-eperm`;
- SaBRe `c-programs/keyctl-enosys`.

The six cells not executed were ptrace verify cells in
`determinism-stress-c`: `lock-free`, `pid-tid`, `pid-tid-identity`,
`pipe-prefill`, `producer-consumer`, and `signal-order`. A focused producer
attempt was refused before running cells because its nested containment
readback failed. A later supplemental bench attempt was also correctly
refused because it lacked a pre-created run record. Neither refusal contributes
evidence; the six remain `NO VERDICT`.

## Green delta at the serialization boundary

**172/172 legacy green -> 149/172 canonical green: delta -23 green cells.**

No row was phased, excluded, or allowed to fall back to the legacy comparator.
The switch reached all 5/5 compatibility modes and all 167/167 selected verify
cells. The earlier strict flag landed at 2026-08-05T03:52:37Z; this measurement
completed at 2026-08-12T10:19:39Z: **7 days 6 hours 27 minutes of human calendar
time, spanning eight UTC dates**.

This candidate changes the registered validation producer, so this run cannot
self-vouch and did not mint a qualifying receipt. It is scorecard measurement,
not landing authority. Terminal disposition of PR #2164 belongs to
`codex-reviewA`.

## Reproduction

Derive the declared matrix from the versioned manifests:

```sh
python3 experiments/manifest_backend_matrix_derivation/derive.py \
  hermit c6c6d6958a8dc940419ead146c8283f06607a9df
```

Read the selected ratchet:

```sh
jq '.cells | length' hermit/ci/expected-e2e-plan.json
```

Read the authoritative receipt:

```sh
./ci-hub/ci-hub validate-status \
  --sha c6c6d6958a8dc940419ead146c8283f06607a9df --json
```

The comparator switch is also a measurement-basis change for wall-time
history. Do not compare pre-switch and post-switch wall times as one series
without this boundary annotation. The existing wall-time gate correctly
refuses a regression verdict while retention and conditioning are inadequate.
