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
- an accountable owner and an accountable record in the `task` field: either
  an ORC TaskGraph task id, or a full GitHub issue URL
  (`https://github.com/<owner>/<repo>/issues/<n>`) for agents that do not use
  TaskGraph. An issue URL is checked through `gh issue view`: an open issue
  keeps the row accountable, a closed one counts exactly like a closed task,
  and a failed lookup is `unverifiable`, never drift;
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
- `gated` — no implementation yet, deliberately deferred on the named `gate`
  condition, and carried by a nonterminal accountable task. The ledger keeps
  gate text as historical provenance, but the checked report retires it when
  fresh ancestry proves an implementation or when the accountable task closes;
  stale text therefore cannot keep reporting a block nobody is advancing;
- `unaccountable` — the row is NOT satisfied and its accountable task (an ORC
  TaskGraph task or a GitHub issue) is `CLOSED`, so nothing will surface the unmet obligation (genuine drift). Task lookup
  tests existence, and a closed task exists; measured 2026-08-08, 3 of 21 rows sat in
  this state. A closed task on a `satisfied` row is expected; a closed task with
  no ancestry-confirmed implementation is unaccountable even if historical
  gate text remains in the ledger.

  **`CLOSED` is one bit with two meanings, so the report also says which.** A task
  closes because the work was finished, and a task closes because its tracking moved
  elsewhere, was superseded, or was reconciled after its agent died — and on
  2026-08-20 BOTH drifting rows were the second kind, one closed by "Moved to
  hermit issue #1857" and one by dead-agent slot reconciliation. The per-record
  `closure_claim` field and the terminal `closure_claim=<id>:<recorded>` line carry
  what the closure itself recorded, in the words `ci-hub/closure/verified_close.py`
  writes: `landed`, `implemented-unlanded`, `n/a` (a `CLOSURE-VERIFIED` note
  predating the landing field), `implemented` (the tag, no gateway note),
  `none-recorded` (the closure asserted no completion), or `unread` (this run could
  not look — a no-result, never reported as if the closure had been silent).

  **Both remain one state, deliberately.** A closure claiming completion contradicts
  fresh ancestry and needs the claim withdrawn or the work re-landed; a closure
  claiming nothing retired the tracking and needs the obligation re-tasked. Two
  remedies, but equally unmet — giving one its own state is how it later gets
  filtered out of the drift count. Neither value can satisfy a row:
  `ci-hub/closure/README.md` lists both the `closed` status and the `implemented`
  tag as NON-AUTHORITATIVE, and only fresh ancestry satisfies a directive;
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
`missing_task`, `task_not_found`, `not_landed`, and `invalid`; `open` and `gated`
are reported separately as in-progress and deferred work. `unverifiable` and
`fetch_failed` are counted as `no_result`, NOT as drift — they mean the checker
could not look, which needs the opposite response to work that regressed. (This
paragraph listed them as drift, and had done since `7af17d80` on 2026-08-09 removed
them from `DRIFT_STATES`; `task_not_found` was likewise missing from the list.)

The hourly `owner_tooling_directives` health tick runs the same command and
wakes the coordinator for every state other than complete fresh-main ancestry.
The versioned seed preserves the 16-row 2026-08-04 audit, expands its
cross-repository legs, and records this tracking directive itself. Parent pins
and child obligations therefore cannot be hidden by one partially landed
directive.
