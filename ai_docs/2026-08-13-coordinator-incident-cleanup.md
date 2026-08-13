# Coordinator incident cleanup, 2026-08-13

## Plain-language outcome

While one owner-directed agent held the Hermit validation and basic-sanity work, another coordinator
continued changing the parent infrastructure and opened five Hermit pull requests. None of the Hermit
pull requests reached `main`, but 69 commits reached `dev-hermit/main` between
`ea96d3a28523af5cd0fb782c95863b0d7c3ec134` and
`2e77f2458053dc785346da3065dc2a6a674aa911`.

This cleanup restores the parent infrastructure, policy, scripts, reports, and experiments to the
pre-incident tree. It does not rewrite history: all 69 commits remain ancestors of this cleanup and
can be inspected or selectively recovered. It preserves the append-only validation ledger rows and
the current Reverie `main` snapshot. It also keeps one short owner-directed rule: the author owns work
through landing and does not close a task merely because a pull request exists.

## Measured blast radius

The net incident diff against `ea96d3a` was 131 files, 28,810 inserted lines, and 802 deleted lines.
It included 46 `ci-hub` paths, 43 experiment paths, 21 documentation paths, 11 script paths, five
coordinator-skill paths, two ledger shards, one worktree registry, the parent `AGENTS.md`, and the
Reverie gitlink. Much of the line count was generated console output committed under two experiments.

The changes were not active in the primary parent checkout on `devbig014`, which remained at
`ea96d3a`. Therefore they produced no improvement in the Hermit validation runs or scorecard work
being performed there. Several commits addressed plausible bookkeeping defects, but they expanded
the machinery that decides whether evidence may be counted rather than improving Hermit's measured
deterministic-execution envelope.

## What this cleanup keeps

- `ledger/hermit/devbig014/2026-08.jsonl` and `ledger/hermit/devbig030/2026-08.jsonl`, because failed
  and incomplete validation attempts are still historical evidence and the ledger is append-only.
- The Reverie gitlink at `ee6716a65d41e8f1d65ee32efa4aafa910b9cf29`, which exactly matched fetched
  `rrnewton/reverie:main` during the audit.
- A concise author-owns-landing rule in `AGENTS.md` and the coordinator summary, replacing both the
  old close-at-publication rule and the incident's much larger claim-parsing policy.
- This report.

Everything else introduced by the 69-commit incident range is restored to the `ea96d3a` tree.

## Hermit pull-request triage

| PR | Decision | Assessment |
| --- | --- | --- |
| #2242 | Close without merge | Adds 207 lines of GitHub-label read-back around `locally-validated`. Labels are not validation authority, GitHub Actions are off, and this does not improve the scorecard or product. |
| #2243 | Close without merge | Adds a 254-line shell parser that infers Rust caller reachability from source text while grandfathering 17 unresolved claims. It is brittle, overlaps the manifest/scorecard overhaul, and measures metadata rather than execution. |
| #2244 | Close without merge | Contains a useful observation—that DBT compares stdout, termination, and selected summary values—but packages it in the rejected assurance-tier matrix and reports 26/28 rather than the owner's binary per-cell scorecard. The observation can inform later DBT work; the implementation should not be merged. |
| #2245 | Close without merge | Tightens reuse of historical validation rows. The predicate is defensible, but the owner requires fresh scorecard results on each validation; expanding cache-admission machinery moves in the wrong direction. |
| #2246 | Keep draft | The 16-line `waitid` change addresses a real ptrace liveness defect with a strong before/after measurement. It has no automated regression test and no qualifying full validation, so it remains draft until the single Hermit workstream can add the subprocess-timeout regression and validate it. |

Hermit `origin/main` remained `1f4e430c135204591daa0c0cba84f77dae679a78` throughout the audit. None of
these five branches changed `main`.

## Value assessment

The incident did uncover three facts worth retaining as observations:

1. A successful label-reconciliation command does not prove a label is present.
2. Historical rows without an expected-gate count cannot prove complete execution.
3. Blocking `waitid` can starve logical-time progress; #2246 measured an indefinite spin before the
   fix and a 0.27-second `EINTR` completion after it.

The first two are properties of administrative machinery that is being simplified and do not justify
their added code. The third is a product defect and is preserved as a draft branch. No incident commit
increased the committed Hermit compatibility score, produced a qualifying Hermit receipt, or changed
Hermit `main`.

## Verification of the cleanup

The cleanup is built in a disposable clone from exact incident tip `2e77f245`. Verification requires:

- all `ci-hub`, general script, experiment, report, and skill paths to match `ea96d3a`, except the two
  concise task-lifecycle policy files, their focused test, and this report;
- the two ledger shards to match `2e77f245`;
- the Reverie gitlink to match `2e77f245` and fetched Reverie `main`;
- the task-lifecycle focused test and unchanged ledger/health tests to pass; and
- the command-line front door to build and print help without warnings.

Observed in the disposable clone:

- `python3 scripts/test_task_lifecycle_policy.py`: 2/2 passed.
- `python3 ci-hub/ledger/tests/test_validate_rows.py`: passed.
- `python3 ci-hub/health/tests/test_operational_health.py`: 34/34 passed.
- `./ci-hub/ci-hub --help`: exit 0, 62 stdout lines, zero stderr lines.
- `python3 ci-hub/validate/tests/test_start_unit.py`: 24 passed and 17 failed. All 17 failures are
  the pre-existing fake-runner omission recorded by incident commit `8629fbfc`: the fake rejects the
  existing Herdr capacity query before reaching each test's subject. That incident commit explicitly
  reported the same 17 failures. This cleanup does not modify either launcher file relative to
  `ea96d3a`; the failure is retained baseline debt, not a cleanup regression.

After publication, the primary checkout must not be fast-forwarded over unrelated dirty files until
those files have a recovery commit. Landing and activation are separate facts.
