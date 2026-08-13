# Parent path ownership reconciliation, 2026-08-13

This report preserves the assertions removed from the active parent-path ownership
list. It does not transfer ownership, declare a path free for writing, or clear any
recorded evidence.

## Sources checked

- At 2026-08-13 03:42 UTC, `hermit-coord` read the live orchestrator registry and
  reported 13 live agents: `hermit-901` through `hermit-909`,
  `hermit-bpf-research`, `hermit-consistency`, `hermit-coord`, and
  `hermit-deslop`.
- `hermit-coord` initially reported its own three parent-path rows current, then
  withdrew that conclusion after the task checks showed that agent liveness alone
  did not establish continued need. It assessed the three path sets separately
  and explicitly released all three grants. It reported `receipt-repair`,
  `scorecard-fixer-codex`, and `strict-metric` absent from the live roster.
- Eight exact `tg show TASK` queries against the active `hermit3` TaskGraph
  returned `Task ... not found`. The canonical same-lineage archive check then
  queried `hermit1`: the three `strict-metric` tasks exist there with status
  `IN_PROGRESS`; the other five do not exist in either known database.
- The live registry also distinguishes existence from availability:
  `hermit-consistency` exists but is deliberately parked outside work assignment.
  This report therefore does not treat a present agent as necessarily available,
  or an unavailable agent as absent.

## Former assertions preserved

The following rows no longer belong in the active ownership list. No replacement
owner was supplied. Moving these assertions to history does not authorize another
agent to write the paths; a future writer still needs an explicit assignment.

| Former agent | Former task or tasks | Former branch | Former paths | Measured state |
| --- | --- | --- | --- | --- |
| `receipt-repair` | `dev-hermit-pr64-receipt-authority` | parent `main` | `ci-hub/ci-hub.rs`; `ci-hub/lib/qualifying_receipt.rs`; `ci-hub/lib/records.rs`; `ci-hub/lib/validate_status.rs`; `ci-hub/qualifying_receipt.py`; `ci-hub/validate/anchor_select.py`; `ci-hub/validate/finalize_receipt.py`; `ci-hub/validate/qualifying-receipt.json`; `ci-hub/validate/test_finalize_receipt.py`; `ci-hub/validate/tests/test_anchor_select_clause_brackets.py`; `ci-hub/validate/tests/test_qualifying_receipt_mutation.py`; `ci-hub/validation/test_verify_receipt.sh` | Agent absent; task not found in active or known same-lineage archived TaskGraph; no current owner. The available task records cannot distinguish a never-recorded task from a record omitted during an earlier cutover. Former mechanism: `typed-coverage-receipt-authority`. |
| `hermit-coord` | `worktree-prune-incident-ledger` | `codex-coord@d93db59` | `ai_docs/2026-08-05-worktree-prune-incident-ledger.md` | Agent live; task not found in active or known same-lineage archived TaskGraph; owner reports the work complete and landed at `cb646b8c97b65ea9e07c89265a3d709d0670c706`; ancestry was verified against current parent main `840e5bcc333d110cb61cea37119b2a872882a7f2`; owner explicitly released the grant. No current owner. Former mechanism: `release-worktree-targeted-child-removal`. |
| `hermit-coord` | `drainer-5-ownership-transfer` | `codex-coord@423cb3e` | `worktrees/ARCHIVED.md` | Agent live; task not found in active or known same-lineage archived TaskGraph; owner reports the task-specific transfer finished and the later registry append landed at `7788350f9e5774c1f79e0c2427bc830111e8f8b5`; ancestry was verified against current parent main `840e5bcc333d110cb61cea37119b2a872882a7f2`; owner explicitly released the grant. No current owner. Former mechanism: `worktree-registry-owner-transfer`. |
| `hermit-coord` | `checkout-pr-pin-repo-binding` | `codex-coord@1f67ef5` | `scripts/checkout-hermit-pr-latest-reverie.sh`; `ci-hub/tests/test_checkout_pr_pin_binding.py` | Agent live; task not found in active or known same-lineage archived TaskGraph; owner reports replacement task `pr-prep-script-forces-pin-to-reverie-tip` closed with verified closure and commit `570c4e32345159dd8ecd43e7bde834bddd6d64ad` landed and activated; ancestry was verified against current parent main `840e5bcc333d110cb61cea37119b2a872882a7f2`; owner explicitly released the grant. No current owner. Former mechanism: `latest-reverie-pin`. |
| `scorecard-fixer-codex` | `scorecard-detlog-tier-vocabulary` | parent `main` via isolated clean checkout | `compat-envelope/check-scorecard-tier.py`; `compat-envelope/render-scorecard.rs`; `compat-envelope/README.md`; `compat-envelope/tests/test_check_scorecard_tier.py`; `compat-envelope/tests/test_render_scorecard_observable_schema.sh`; `ci-hub/tests/test_published_scorecards_are_tiered.py` | Agent absent; task not found in active or known same-lineage archived TaskGraph; no current owner. The available task records cannot distinguish a never-recorded task from a record omitted during an earlier cutover. Former mechanism: `scorecard-evidence-tier`. |
| `strict-metric` | `measure-strict-compat-green-cell-drop`; `restore_fullcorpus_performance_fixtures`; `resolve-fullcorpus-timed-progress-duplicate` | parent `main` plus a Hermit feature branch | `experiments/strict_compat_green_cell_drop_20260806/**`; `compat-envelope/corpus/corpus-nonc.tsv`; slot-local `hermit/tests/e2e/performance/**`; required current-schema manifest and inventory registrations | Agent absent; all three tasks exist only in the `hermit1` archive and remain `IN_PROGRESS`, so task completion did not release these paths; no current owner. Former mechanisms: `strict-compat-raw-byte-evidence`, `strict-compat-denominator`. |

## Recommendation

Before anyone relies on a parent-path ownership row, perform one read-only check
that reports the agent's live-registry state, the task's live-or-archived TaskGraph
state, and whether the named owner still needs the path. A row is current only when
the agent exists, the task is live and unfinished, and the owner confirms continued
need. A closed task makes completed work visible; an archived unfinished task makes
a stranded assignment visible; and a missing task makes the absent record visible.
Each requires coordinator reconciliation rather than inference.

The check should also make three agent outcomes visible: present and available;
present but deliberately unavailable; or absent. It must never delete a row,
transfer ownership, or infer that a path is free.

This keeps the explicit-transfer rule intact while making decay visible at the
point where a writer would otherwise trust the prose.

## Publication record

The reconciliation content first landed at
`9e0769b0862d83ec22aa4d5d03d49c4c9117ed0e`. Its commit message contains only
the canonical final machine-attribution line because the writer was invoked with
multiple message arguments although its interface accepts one message. Published
main was not rewritten. The immediate follow-up records the required disclosure,
task, validation, and this invocation defect while retaining the first commit in
linear ancestry.
