# Owner tooling directive obligations

`ledger.json` turns owner tooling, configuration, and repository imperatives
into obligations. A quotation, dispatch, design document, branch, or open pull
request is not completion. `check.py` reports `satisfied` only after the claimed
full commit SHA or pull request `mergeCommit.oid` is an ancestor of the freshly
fetched named target branch.

Run the pure primer before editing the ledger:

```bash
./ci-hub/directives/check.py --quickstart
```

Each record requires:

- the date the directive was asked or first durably recorded;
- the verbatim instruction and its reason in the durable source record;
- for a temporary directive, an explicit expiry or review trigger in that source record. Expiry does
  not erase history; it changes the directive from active policy to a superseded historical record;
- the GitHub repository, local checkout, and target branch;
- an accountable TaskGraph task and owner;
- a full implementing commit SHA or pull request number, or explicit `null`
  while no implementation has been claimed;
- `parent_id` for every cross-repository or incomplete-scope remainder; and
- an optional `gate` naming the external condition a deliberately deferred
  directive is waiting on (for example, "zero open Hermit PRs"). A `gate` must
  name its blocking condition: an empty `gate` is rejected as `invalid`, because
  a bare "gated" is only a quieter form of unknown.

The checked result is written to `ignored/ci-hub/directives/latest.json` with
the resolved implementation SHA, freshly fetched target tip, and terminal
ancestry verdict. The states distinguish what is being handled from genuine
drift, so the signal does not cry wolf:

- `satisfied` / `partial` — ancestry-confirmed on fresh main (partial when a
  child obligation is still incomplete);
- `open` — no implementation yet, but owned and carried by a resolvable task,
  i.e. actively in progress, not drift;
- `gated` — no implementation yet and deliberately deferred on the named `gate`
  condition, not drift. The gate is the durable obligation, so it remains
  `gated` after the finite task that recorded the decision closes; the hourly
  checker continues to surface the named condition until it is satisfied;
- `unaccountable` — the row is NOT satisfied and its accountable TaskGraph task is
  `CLOSED`, so nothing will surface the unmet obligation (genuine drift). Task lookup
  tests existence, and a closed task exists; measured 2026-08-08, 3 of 21 rows sat in
  this state. A closed task on a `satisfied` row or an explicitly `gated` row is
  expected; the predicate keys on the pairing, not on closure;
- `needs_owner` — no accountable owner recorded (genuine drift);
- `missing_task` — no resolvable accountable task (genuine drift);
- `not_landed` — claimed evidence is not an ancestor of fresh main (genuine
  drift);
- `invalid` / `unverifiable` — malformed metadata, or an ambiguous verifier
  verdict, never green;
- `fetch_failed` — the verifier reached the checkout but could not fetch the
  target to compare against (a network/Git/proxy failure). This is kept
  distinct from `not_checked` and from `not_landed`: the checker never reached
  the evidence, so the verdict is genuinely unknown (exit 2), never a clean
  pass. Conflating a broken fetch with "nothing to check" lets a broken checker
  read as green, which is worse than an error.

The `drift=` field of the terminal summary counts `unaccountable`, `needs_owner`,
`missing_task`, `not_landed`, `invalid`, `unverifiable`, and `fetch_failed`;
`open` and `gated` are reported separately as in-progress and deferred work.

The hourly `owner_tooling_directives` health tick runs the same command and
wakes the coordinator for every state other than complete fresh-main ancestry.
The versioned seed preserves the 16-row 2026-08-04 audit, expands its
cross-repository legs, and records this tracking directive itself. Parent pins
and child obligations therefore cannot be hidden by one partially landed
directive.
