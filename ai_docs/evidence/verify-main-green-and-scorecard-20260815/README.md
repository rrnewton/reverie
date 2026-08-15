# Main's green belongs to `1b98ccef5`, not to the head — 2026-08-15

**Produced by `hermit-teardown`.** Promoted by `hermit-coord`; not authored, not re-measured, not
independently reproduced. Source `ignored/evidence/verify-main-green-and-scorecard-20260815/verification.txt`, sha256 `10eaef709dc5949dfc2d56a848be68eb4c1df4889769e2688a1bda9600c113f4`, 44 lines at copy time.

**Why this is tracked.** It is the only record that main's green belongs to `1b98ccef5d5098bbc3e72fba689b1a026aaeb780`
and **not** to the branch head. That fact is load-bearing for every landing decision made today, and a
wrong head SHA was reported hourly for most of the session before this correction.

**Deliberately NOT promoted from the source: the recent-runs listing.** It regenerates and ages
badly, so a tracked copy would be wrong within the hour. That is teardown's own recommendation and it
is the right call — the same live-source staleness problem that stranded a 62-line tracked copy of a
141-line file earlier today. Read the listing live instead.

## Anchors, and one of them has already moved

| | |
|---|---|
| head the evidence describes | `61b9ba7aa4dd763c1bda091316852d5290f9466c` |
| head carrying the green | `1b98ccef5d5098bbc3e72fba689b1a026aaeb780` |
| hermit `origin/main` at promotion time | `302a1a9c0fde564db0292d1ea5cabc91343e79bc` |

**MEASURED BY hermit-coord at promotion time:** hermit main advanced to `302a1a9c0`
("Consolidate manifest Rust compilations") while this was being promoted, so `61b9ba7aa` is now a
superseded head — it is an ancestor of the new tip. **The finding survives the advance:** the
scorecard total is identical at both, `170 / 5206 / 5376`, and `302a1a9c0` carries no validate
record of its own. So the green still belongs to `1b98ccef5` and still does not belong to the head.

## Authoritative validate-status, verbatim
# validate NOT-VALIDATED 61b9ba7aa4dd763c1bda091316852d5290f9466c -- no clean full-coverage PASS record (0 non-qualifying record(s) for this commit); origin/main carries no record for it either, so this absence IS authoritative

# validate VALIDATED 1b98ccef5d5098bbc3e72fba689b1a026aaeb780 (passed 2026-08-15T00:24:18Z, wall 1001s, host devbig014, profile full/full) -- clean-tree commit-anchored full run

## Per-main-commit verdict and scorecard
61b9ba7aa4dd  none  **Total** | **170** | **5206**
5465e1451c37  none  **Total** | **170** | **5206**
d6856c1f95f5  none  **Total** | **170** | **5206**
1b98ccef5d50  VALIDATED  **Total** | **170** | **5206**

## Raw ledger counts
rows scanned across both hosts: 1698; commit == 61b9ba7aa: 0; base_sha == 61b9ba7aa: 3 (all exit_code 1)
retained run records under ignored/validate/runs/: 201; mentioning the SHA: 0

## Scorecard at 61b9ba7aa, per-backend rows
| Backend | Green | Red | Total |
| --- | ---: | ---: | ---: |
| `ptrace` | 150 | 858 | 1008 |
| `dbt` | 9 | 999 | 1008 |
| `kvm` | 0 | 1008 | 1008 |
| `sabre` | 9 | 999 | 1008 |
| `liteinst` | 2 | 1006 | 1008 |
| `native` | 0 | 336 | 336 |
| **Total** | **170** | **5206** | **5376** |

