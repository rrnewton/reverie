# CI-signal vision completion audit — 2026-08-30

Audit time: `2026-08-30T14:30:30Z`

This is a read-only completion audit of
`vision-ci-signal-is-trustworthy-end-to-end`. It checks current repository
state rather than treating TaskGraph closure or an implementation branch as
delivery evidence.

## Frozen repository state

Fresh fetches produced:

| Repository | `origin/main` |
| --- | --- |
| `rrnewton/hermit` | `d86638f3402c9bfde165440a19bea8d257b457f2` |
| `rrnewton/dev-hermit` | `fc5931dfbea84cffeb9723c36b842195a162682a` |
| `rrnewton/reverie` | `c2f45c842b3fea3fb29a00079b5f9abe487074ac` |

Commands:

```text
with-proxy git fetch origin main --prune
git rev-parse origin/main
```

The primary Hermit checkout was clean and exactly at its freshly fetched
`origin/main`. The primary parent checkout was dirty and behind, so it was not
used as evidence for landed parent source. Parent source was read from
`origin/main`; executable checks used a linked worktree whose `ci-hub/` paths
were byte-identical to parent `origin/main`.

## Verdict

**The vision is not complete.** Four core pieces are landed and materially
working: framework-recorded exact pass counts, explicit validate-status history
scope, framework-local manifest retries, and a cross-source reconciliation
command with an immutable scorecard projection. The end-to-end condition is
still false because:

1. the optional machine-local run-number repair has landed, but the owner's
   literal command still executes the stale dirty primary parent and therefore
   still aborts until that checkout is safely reconciled;
2. the unselected-cell campaign did not activate any cell and did not give most
   red cells a current measured verdict;
3. no valid clean concurrent pair plus same-SHA solo baseline exists;
4. current Hermit main has no qualifying full receipt and is 17 first-parent
   commits beyond the last hard green;
5. the independent final certification task is open and has no verdict.

The TaskGraph subtree reports `8/15 complete`, but the more important problem
is substantive: `drive-the-unselected-cells-to-a-measured-verdict` is marked
closed even though its stated acceptance condition is not met.

## Requirement-by-requirement audit

### 1. Exact framework-recorded `TESTS-PASSED`

**Status: landed and demonstrated.**

Coordinated carriers:

- Hermit producer: <https://github.com/rrnewton/hermit/pull/2839>, landed as
  `4dbadc85679b619ca91551bbc4bc66c0611cc0f2`.
- Parent consumer: <https://github.com/rrnewton/dev-hermit/pull/226>, landed as
  `569f30a2d4538daa46a33f1579d901b17bbcf396`.

Current parent source consumes `row.passed_tests` directly and explicitly says
it will not reconstruct the value from presentation text or from
`executed_tests - failures`. A passing receipt carrying `passed_tests` is
refused unless it equals `executed_tests`.

Commands:

```text
git -C /home/newton/work/dev-hermit grep -n "recent_row_passed_tests" origin/main -- ci-hub/ci-hub.rs
git -C /home/newton/work/dev-hermit show origin/main:ci-hub/lib/qualifying_receipt.rs
```

A current-main parent reader against the last qualifying Hermit receipt reports:

```text
sha             c98dc60b23b8bd7238a94c3a0f93cc425cf8c6fe
verdict         VALIDATED
gates           76 / 76
executed_tests  2194
passed_tests    2194
filtered_tests  499
skipped_nodes   0
receipt_digest  b3994e267c937a2661b793d91d4357f4691f1778e8060184d8a1e58c7fb0643a
```

This proves the displayed passing count is exact for that run. It does not
prove current Hermit main is green.

TaskGraph:
`tests-passed-must-be-an-exact-count-from-the-framework` is closed and tagged
`implemented,landed`.

### 2. Trustworthy validate-status history and provenance

**Status: the history-scope and optional-counter fixes are landed; the owner's
ordinary command is still broken because its executable comes from a stale
dirty primary parent checkout.**

The original ambiguity was fixed by parent
<https://github.com/rrnewton/dev-hermit/pull/225>, landed as
`6f78ca848294b6abe578bb1d7b3a6ef4e4c5b47e`. Current source:

- prints the exact history ref and tip used for `COMMITS-SINCE-GREEN`;
- says the recent-run table includes completed rows from all branches;
- points callers to `--commit-timeline` for the `origin/main`-only view.

Command:

```text
git -C /home/newton/work/dev-hermit grep -n \
  -E "history_ref|history_tip|not restricted to|commit-timeline" \
  origin/main -- ci-hub/ci-hub.rs ci-hub/validate/README.md
```

The next defect was that parent main required `admission_result.run_number` on
every admitted validation handle. The exact offending retained record is:

```text
/home/newton/work/dev-hermit/ignored/validate/runs/
  validate-mega-lander-677df8ff06dc-1787940216180430273-2048177-8bd05b85.json
```

It is a completed successful validation handle whose `admission_result` has
`state=admitted` and `recorded_at`, but no run number. Parent
<https://github.com/rrnewton/dev-hermit/pull/248> repaired this and landed as
`fc5931dfbea84cffeb9723c36b842195a162682a`. The reviewed head was
`a166b669832ff2e19b55ee367e3d230f337627a5`; all 12 changed paths are
byte-identical between that head and the landed commit.

Current main now treats the number as optional display metadata scoped by
machine shortname; the durable handle/run ID, repository, SHA, and wrapper
result remain the identity. Future validations still allocate positive numbers
under the per-repository/per-machine lock. Historical rows are not renumbered.

This code landing has not yet repaired the command the owner actually invokes.
The primary parent remains dirty and at `35d6362ac957...`, so its executable is
still the pre-fix copy. Consequently the literal read-only command still exits
2 before returning status:

```text
cd /home/newton/work/dev-hermit/hermit
../ci-hub/ci-hub validate-status
```

The repaired tool, run against the same canonical state before landing,
returned the normal table and reported current Hermit main, last green c98, and
17 commits since green. Exact-head tests were `398/398` for the Rust-script
suite, with focused `run_registry`, `start_unit`, `signal_crosscheck`,
owner-watch-correction, and `validate_rows` suites green. Those establish the
fix; safe reconciliation of the dirty primary is the remaining activation
step.

TaskGraph:

- `validate-status-reports-a-green-and-a-depth-that-do-not-match-mains-history`
  is correctly closed/landed.
- `validate-status-refuses-with-a-missing-run-number-on-the-admission-result`
  still reads in progress/implemented in TaskGraph even though PR248 is now on
  main; its lifecycle record and primary-checkout activation lag the landing.

### 3. Remove validate's outer retry for manifest cells

**Status: landed and structurally present on current Hermit main.**

Carrier: <https://github.com/rrnewton/hermit/pull/2842>, landed as
`f2be3d81c9cb609443bd34c4611683e2bfccf728`.

Current source gives each selected cell at most two framework-owned attempts in
`test-harness`, rejects attempt ordinals above that cap, and removes every typed
manifest node from the later scheduler retry graph regardless of whether its
first graph result was failed, dependency-skipped, aborted, or unreported.
`E2E_ATTEMPT` remains only in assertions proving it is absent; it is no longer
an execution-path producer.

Commands:

```text
rg -n "E2E_ATTEMPT|retry_candidate_tags|MAX_ATTEMPTS_PER_CELL" \
  scripts/validate.rs ci/manifest-plan/src
nl -ba scripts/validate.rs | sed -n '15539,15560p;18613,18744p'
nl -ba ci/manifest-plan/src/runner.rs | sed -n '1371,1435p'
```

TaskGraph:
`remove-the-outer-retry-loop-in-validate-rs-and-land-it` is closed and tagged
landed.

### 4. Unselected cells: measurement truth and activation

**Status: not complete, despite the task being closed. Zero cells moved.**

The useful tool change is Hermit
<https://github.com/rrnewton/hermit/pull/2798>, landed as
`e277d771f25d8ef7715ebc6a14f4d1f1a13bf00d`. It makes repeated-red selection
possible and keeps repetition separate from population selection. It changes
only `ci/compat-envelope/pressure-test.rs`; it neither changes the scorecard nor
promotes cells.

The status counts are identical immediately before PR2798, at PR2798, and at
current main:

```text
e277d771^  green=304  red=372  not-applicable=5068
e277d771   green=304  red=372  not-applicable=5068
d86638f3   green=304  red=372  not-applicable=5068
```

Command:

```text
for rev in e277d771^ e277d771 d86638f3; do
  git show "$rev:ci/compat-envelope/cells.json" |
    jq '[.cells[].status]|group_by(.)|map({key:.[0],value:length})|from_entries'
done
```

Fresh current-main checks give:

```text
./tests/manifest-cli.rs list
  manifest-cli: 359 test(s) listed

./ci/compat-envelope/scorecard.rs check
  compatibility scorecard: tracked table and 5744 comparable cells are current;
  selected regression denominator 307 = 304 comparable + 3 custom
```

Current scorecard truth is:

| Status | Never measured | Measured and passed | Diverged-unlocated | Diverged | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| green | 0 | 294 | 6 | 4 | 304 |
| red | 362 | 2 | 0 | 8 | 372 |
| not-applicable | 5067 | 0 | 0 | 1 | 5068 |

Thus colour still means plan membership, not a measured outcome. Current
`SCORECARD.md` says so explicitly. Ten selected/green cells carry a divergent
measurement, while two unselected/red cells carry a passing measurement.

The two red/pass cells are **not safe activation candidates yet**:

- `applications/example-timed-progress-bar`, `verify/ptrace`: one pressure-test
  pass at Hermit `4cd48cbe...`, one stored observation.
- `c-programs/nanosleep-par`, `verify/sabre`: one validate pass at Hermit
  `d77c8d0b...`, one stored observation.

Both observations are old and single-sample. The task's own established
qualification rule requires repeated current canonical verification and treats
any retry or retained non-pass as disqualifying; one pass proves neither.

There is also an unlanded, conflicting vocabulary change,
<https://github.com/rrnewton/hermit/pull/2738>, that renames green/red to
selected/not-selected. It does not substitute for running and classifying the
372 red cells.

TaskGraph:
`drive-the-unselected-cells-to-a-measured-verdict` is closed/tagged landed, but
its verification text requires the green count to move and every remaining red
cell to have a reason established by running. Current main proves neither.

### 5. Cross-check scorecard, ledger, receipts, run handles, and GitHub status

**Status: the mechanism and immutable scorecard projection are landed; the
current tip reconciles as unvalidated, not green.**

Carriers:

- Parent <https://github.com/rrnewton/dev-hermit/pull/227>, landed as
  `0ab6b31effb0dc200fd88a9037c47cbea308239e`, added the read-only exact-SHA
  cross-check.
- Hermit <https://github.com/rrnewton/hermit/pull/2840>, landed as
  `c98dc60b23b8bd7238a94c3a0f93cc425cf8c6fe`, and parent
  <https://github.com/rrnewton/dev-hermit/pull/228>, landed as
  `928e7e04564a0a1cce1789957d69fecf4f8c2bcc`, added projection-frontier
  comparison.
- Hermit <https://github.com/rrnewton/hermit/pull/2863>, landed as current main
  `d86638f3402c9bfde165440a19bea8d257b457f2`, made projection independent of
  local Git-object availability and regenerated it from the landed series.

A fresh component-by-component execution of current parent `signal_crosscheck`
against current Hermit main produced `AGREE`, with no unavailable source and no
issues. The agreement is an honest negative result:

```text
validate-status  NOT-VALIDATED, qualifying_count=0
ledger           exact_rows=0
series           exact_rows=0
run handles      matching_handles=[]
GitHub status    no exact "Local validation" status
scorecard        source_commit=fdcec0b4..., source_tree=e15c056b...,
                 rows_read=13422, relation=current
```

The scorecard source and current published source both contain 13,422 canonical
events. The local spool adds 615 rows, 308 of which duplicate published events,
for 13,729 logical series events; that broader live union is reported
separately and is not confused with the committed projection source.

This demonstrates that the landed sources can agree that no validation exists.
It still needs a positive current-tip run after PR248 lands to prove the same
path reconciles a new qualifying receipt, its run handle, scorecard frontier,
and published status.

TaskGraph:
`crosscheck-every-source-of-ci-signal-against-every-other` is closed and tagged
implemented/landed.

### 6. Concurrent and efficient current validation

**Status: partial infrastructure improvements landed; the required experiment
is not complete.**

Landed foundations include:

- <https://github.com/rrnewton/hermit/pull/2732>, landed as
  `3e284e45580518dc4b8a9ccebccfcce7ed2b9488`, replacing a spurious 15-second
  KVM cap with the measured per-cell bound.
- <https://github.com/rrnewton/hermit/pull/2824>, landed as
  `3047712bd0f5149bcf2037d492e89ec36b826645`, keeping framework timeout
  evidence live.
- Parent `0368a10c18da112391eaaa5b44323eb89cfde67a`, which removed identified
  shared-path collisions needed for concurrent validates.
- PR2798 provides bounded per-cell red repetition with a shared build.

A full hard-green run is demonstrably obtainable: the c98 receipt completed in
1,301 seconds (21m41s), ran all 76 nodes, skipped none, and reported exact
2,194/2,194 passing tests. But current-tip green is not routine: current Hermit
main is 17 first-parent commits beyond c98 and has no exact receipt.

The required concurrency proof does not exist. The earlier two-arm run had one
arm broken throughout by a TMPDIR mount condition. There is no clean two-arm
pair plus a same-SHA, same-host, same-launcher solo baseline. Task
`concurrent-validate-must-work-well` remains `BACKLOG/P0`.

Current resource state at audit time:

```text
systemctl --user list-units 'validate-*' --state=running
  no running validate units

ci-hub validate-lock status
  slot 0 FREE
  slot 1 FREE
```

There is still no `validate.rs list quick|full|super` command. `--help` explains
the profile names, `manifest-cli list` exposes 359 manifest tests, and the
scorecard exposes the 307-cell full regression denominator, but one command
does not yet enumerate exact per-profile membership.

### 7. Current-tip hard green and `commits-since-green = 0`

**Status: not complete.**

Current Hermit main:
`d86638f3402c9bfde165440a19bea8d257b457f2`.

The current parent reader returns for that exact SHA:

```text
verdict=NOT-VALIDATED
qualifying_count=0
failed_record_count=0
disqualified_count=0
```

The last qualifying hard green is c98, and it is an ancestor of current main:

```text
git merge-base --is-ancestor c98dc60b... origin/main   # rc=0
git rev-list --first-parent --count c98dc60b...origin/main
17
```

PR248's repaired branch reader independently reports the same `17`, but that
reader is not yet on main. Therefore the owner's completion metric is not zero.

### 8. Independent final adversarial certification

**Status: not started and cannot honestly pass yet.**

Task
`adversarial-review-certifying-the-ci-signal-vision-is-landed` is `OPEN/P0` and
has no notes or verdict. Its acceptance rule requires every component to be
checked by content on `origin/main`, every consumer enumerated, and every
possible goalpost weakening examined. The missing activation campaign,
concurrency proof, PR248 landing, and current-tip hard green prevent
certification today.

## Additional direct vision-linked defects

These are not substitutes for the six core components, but TaskGraph currently
links them directly to the vision and they affect operational trust:

- `owner_watch_timer_executes` is `IN_PROGRESS/P1`. Parent
  <https://github.com/rrnewton/dev-hermit/pull/247> at
  `b52090752bf1393987a9c211ed82026e60095b3a` is open, not landed. Until reviewed,
  landed, installed, and started from the immutable path, the timer must remain
  stopped.
- `initialized_submodules_briefly_appear` is `BACKLOG/P0`. An initialized
  checkout has transiently appeared uninitialized to `pre.submodules`, making
  a full run incomplete.
- `test_strict_compat_retry` is `BACKLOG/P0`. A second attempt has failed in
  0.2s while joining its dagrun cgroup, distinct from the product result of the
  first attempt.
- `one-unreadable-record-aborts-an-entire-query-instead-of-being-isolated` is
  `BACKLOG/P1`. PR248 fixes the known missing-label shape, but it does not prove
  the broader property: one genuinely malformed current handle can still abort
  an entire query rather than being named as indeterminate while other verdicts
  are returned.

Already-landed supporting fixes include parent
<https://github.com/rrnewton/dev-hermit/pull/238> at
`35d6362ac95732beb497f5d6ee09feec21787586` for immutable post-run status reads,
and Hermit <https://github.com/rrnewton/hermit/pull/2854> at
`1dc6cec4c46aa86764b516bd87b9522fbb156249` for the non-repository tool-root
self-test.

## Shortest credible path from here

1. **Safely reconcile the dirty primary parent to landed PR248**, without
   discarding its unrelated work, then rerun the owner's literal command from
   the Hermit checkout. The 12-path landed-content comparison is already zero.
   Do not fabricate a historical number; absence is the honest value because
   numbering is machine-local and the old chronology is not uniquely
   recoverable.
2. **Use the repaired status/run census and launch one full validate at the
   actual Hermit tip**, holding Hermit main steady. Both validation slots were
   free at audit time. The last comparable green took 21m41s, so this should be
   treated as roughly one full-run interval, not an unbounded investigation.
3. **Make that qualifying run the predeclared solo concurrency baseline**, then
   immediately launch two clean full runs at the same SHA, host, profile, and
   launcher conditions while both slots are deliberately reserved. Require
   start skew at most 60s, overlap at least 80% of the shorter arm, identical
   plans, both green, no retries/infrastructure failures, pair-window saving at
   least 10% versus twice the solo wall time, and per-arm CPU within 15% of the
   solo result. This completes the experiment with only two additional wall
   intervals after the needed hard-green run.
4. **Run the red-cell campaign rather than closing on its launcher.** At
   minimum, remeasure the two single-observation red/pass cells at current main
   with repeated canonical checks before considering promotion. Then run the
   repeated-red population, emit its series rows, regenerate the scorecard, and
   review explicit promotions and measured reasons. PR2798's command is the
   instrument; it is not the result.
5. **Review/land/deploy PR247** before restarting the owner-watch timer, and
   preserve the two intermittent P0 validation defects as explicit failures if
   they recur.
6. **Run the independent final certification only after all preceding facts are
   on main**, then require a fresh exact-tip cross-check and a qualifying green
   receipt with `commits-since-green = 0`.

## Bottom line

The direction is materially better: the system now records exact pass totals,
states which history it queried, keeps manifest retries at cell scope, exposes
the complete 307-cell selected denominator, and can reconcile current sources
without pretending absent evidence is green. But the vision's decisive claims
remain untrue today: current-tip green is absent, most red cells remain
unmeasured, no cells were activated by the purported activation task, clean
concurrent operation is unproven, the owner-facing status command still runs a
stale pre-fix executable, and no independent final certification exists.
