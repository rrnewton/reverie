# Validation ledger

Append-only, Git-backed history of validation runs, sharded per team and
machine. Schema: `ai_docs/validate-ledger-team-machine-schema_20260807.md`.

```
ledger/<team>/<short-host>/<YYYY>-<MM>.jsonl
```

Two machines never write the same path, so the usual case merges trivially in
Git. The only contended file is one host's current month, and local `flock`
serialises that. The host component is a **short** name: an FQDN is an
infrastructure detail that does not belong in a tracked file, and since the
path is also the authority for the event's `host` field, the two cannot
silently disagree.

## Modules

| module | role |
| --- | --- |
| `ledger.py` | schema semantics — parse, lint, union, fold, query, append |
| `publisher.py` | getting events into shared Git history without losing any |

`ledger.py` and its 38 invariant tests landed first, in
`ci-hub/ledger: deterministic global union and green/timeline/bisect queries`.
This document and `publisher.py` were added on top of it.

`validate_event` is the **linter** and `fold` is the **reader**, deliberately
separate. The linter refuses an enrichment that overwrites a set value; the
reader applies whatever is in the log, in order, without re-litigating it. A
reader that enforced the lint could not replay history that was already
written — and the real corpus contains an eleven-deep enrichment chain.

## Events, not rows

Committed bytes are never edited. A later fact about an earlier run is a new
event referencing it: `run.enrich` supplies something absent, `run.correct`
changes something wrong. So a correction never destroys what was believed at
the time — which is what makes `timeline()` meaningful, and what makes a
rewritten or deleted line a *detectable violation* rather than an ordinary
update.

## Live producer and reader boundary

The tracked shards above are also the live source of truth. Hermit and Reverie
send each completed HistoryRow to `validate_rows.py record`; it converts the row
to the versioned event envelope, redacts workspace-local paths, and atomically
appends one whole line to `ledger/<team>/<host>/<month>.jsonl`. Every ci-hub
consumer calls `validate_rows.py rows` (directly or through the Rust front
controller), which unions every shard and exposes the compatibility HistoryRow.

There is deliberately no checkout-local, ignored, discovered-parent, or
environment-selected receipt ledger. Test fixtures may inject an isolated file
only behind their explicit test-mode guard; a real validate has one destination.

The publisher below remains the serialized Git transport for events created in
an isolated spool. It is not a second readable validation ledger: production
receipt consumers read only the tracked shard union.

## Publishing

```python
from publisher import spool, publish
spool(spool_dir, team, host, events)     # durable locally, before any Git work
publish(repo, spool_dir)                 # returns commit + ancestry evidence
```

Three properties, in the order they matter:

1. **A local event is never lost.** Events hit the spool before any Git
   operation, and a spool entry is dropped only once its commit is confirmed to
   be an *ancestor of the freshly-fetched branch*. A push exit code is not that
   confirmation: a push can succeed against a ref that is then replaced.
2. **A concurrent publisher never costs a row.** A rejected push is retried
   with an **append-aware merge** — re-read the remote shard, keep every remote
   line in place, re-append only what is not already there. Never
   `checkout --theirs`, never a whole-file replacement.
3. **No row is ever rewritten.** `verify_append_only` runs against the remote
   content before each push; a non-extension is refused rather than forced.

## Tests

```
python3 -m pytest ci-hub/ledger/tests/ -q
```

`test_ledger_invariants.py` (38) pins the schema semantics and was written
before this implementation. `test_ledger_publisher.py` (13) pins the publishing
behaviour the schema does not describe.

Three of those publisher tests exist because a **mutation sweep** caught tests
that passed without exercising what they claimed.

* **The truncation check was unexercised.** `test_truncated_final_line_...`
  passes even with the check disabled, because its fragment is also invalid
  JSON and `parse_line` rejects it first. Measured: disable
  `read_shard`'s newline check and all 38 invariant tests still pass, while
  `test_a_valid_final_line_without_a_newline_is_still_truncated` fails. A final
  line that is *valid JSON but unterminated* is the only case that binds it,
  and a writer killed between its write and its newline produces exactly that.
* **The push retry was unexercised.** The pre-push fetch means an ordinary
  concurrent append never actually causes a rejection, so removing the retry
  left the suite green.
* **The ancestry gate was unexercised.** Nothing in the happy path can make
  ancestry disagree with a successful push.

The last two are now forced deterministically with server-side hooks on a bare
test remote: an `update` hook that rejects the first push, and a `post-receive`
hook that resets the branch after accepting one.

If you change this code, re-run the sweep: plant a defect and confirm a test
goes red. Currently caught, measured against the landed code — drain-before-
ancestry, no-retry-on-rejection, re-appending an already-published event,
skipping validation before publish, accepting a truncated remote shard, and
disabling the newline check.

## The GitHub per-SHA index (`github_index.py`)

The shards are the durable history, but they answer questions only for machines
that have them. `github_index.py` publishes each validation event as a **commit
comment** so any consumer — including a fresh clone with no ledger at all — can
ask "what happened at this exact SHA?" and get an answer.

**It is a cache. The receipt is the authority.** `verify_receipt` refuses any
entry it cannot bind to a receipt it dereferenced and hashed, so a forged,
stale, or relocated comment is inert rather than believed. Nothing here may be
added to branch protection or read as merge authority.

### Why comments, and not statuses or checks

Decided by measurement in
`ai_docs/2026-08-07-github-validation-index-mechanism-decision.md`, not by taste:

| | statuses | comments | checks |
|---|---|---|---|
| multi-run history | ❌ combined collapses 3 runs to 1 | ✅ all preserved | ✅ |
| rollback | ❌ no delete endpoint, permanent | ✅ deletes cleanly | ➖ |
| offline cache | ❌ pointers only (140-char cap) | ✅ self-describing (65,535) | ✅ |
| feasible on a classic PAT | ✅ | ✅ | ❌ 403, needs a GitHub App |
| least-privilege scope | ✅ `repo:status` | ➖ `repo` | ❌ App + `Checks:write` |

Publish cost was measured and then **struck from the decision**: at the highest
of three defensible receipt rates the dearer mechanism spends 3.46% of one
hour's core budget across a whole day. The honest cost of the choice is the
scope: comments need `repo` where statuses need `repo:status`.

### Two properties that are easy to get subtly wrong

**The receipt sits beside the event, never inside it.** The published payload is
`{index_format, receipt, event}` with the ledger event embedded verbatim. Fold
the receipt into the event and the GitHub copy of an event no longer matches the
shard copy, so `union_events` — which compares bodies for a repeated `event_id`
and raises rather than picking a winner — explodes on every event the machine
published itself. This was a real defect, caught by
`test_an_event_present_on_both_sides_collapses_to_one`.

**Privacy is enforced at the writer, fail-closed.** The prototype learned this
expensively: a read-side check correctly refused to *index* an FQDN that had
already been published. A reader cannot unpublish. `publish()` runs
`ledger.validate_event` — the same verifier that gates shard writes — and raises
before any network call.

### Usage

```python
read = github_index.fetch(sha, repo="rrnewton/dev-hermit", opener=open_receipt)
read.counts        # {'comments_seen': 3, 'accepted': 1, 'rejected': 2}
read.runs()        # folded run facts, same shape as the local ledger

# merge what GitHub knows with what this machine's own shard knows
local = github_index.events_for_commit(shard_paths, sha)
github_index.merge_with_local(read, local)
```

Publishing is **opt-in** (`enabled=True`, wired to `--index-publish`), so
importing the module cannot write. It is idempotent by `event_id`, retries
retryable statuses with bounded exponential backoff, and `unpublish()` withdraws
entries — the rollback statuses cannot do.

`read.counts` always travels with the result: an accepted list alone cannot
distinguish "this commit has one validation" from "one validation and nine
entries I discarded".

### Mutation sweep

11 planted defects, each confirmed to turn a test red: reverting the receipt
split, dropping the write-side privacy gate, dropping target-SHA binding,
treating an unverifiable receipt as a pass, dropping the receipt hash
comparison, dropping `event_id` dedup on read, dropping the idempotency skip on
write, making 404 retryable, dropping the opt-in guard, ignoring local events in
the merge, and ignoring the commit filter in `events_for_commit`.

Every test runs offline against an injected transport. That is not only hygiene:
the prototype's three probe *statuses* are permanently attached to an inert SHA
and cannot be deleted, which is exactly what a network-touching suite reproduces
at CI frequency.
