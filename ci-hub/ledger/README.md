# Validation ledger

**Follow skill `hermit-validation-authority` for validation authority and evidence
qualification.** This README documents ledger storage and publication only.

Append-only, Git-backed history of validation runs, sharded per team and
machine. Schema: `ai_docs/validate-ledger-team-machine-schema_20260807.md`.

```
ledger/<team>/<short-host>/<YYYY>-<MM>.jsonl
```

Two machines publish different tracked paths, so the usual case merges
trivially in Git. Live producers do not write those tracked files; they append
immutable batches below `ignored/ci-hub/validate-ledger-spool/`. The host
component is a **short** name: an FQDN is an
infrastructure detail that does not belong in a tracked file, and since the
path is also the authority for the event's `host` field, the two cannot
silently disagree.

## Modules

| module | role |
| --- | --- |
| `../lib/ledger_event.rs` | complete `validate-ledger/v1` envelope and closed values |
| `check-ledger-events.rs` | Python-to-Rust boundary for the shared envelope type |
| `ledger.py` | storage-context semantics — path/reference lint, union, fold, query, append |
| `publisher.py` | getting events into shared Git history without losing any |

The two production writers are `validate_rows.py record` and
`ledger.py migrate_legacy`. The production readers that can publish or make a
validation conclusion are:

1. `ledger.py union`, for validated shard union;
2. `publisher.py`, for staging, preflight, remote merge, split, and retention;
3. `validate_rows.py rows`, including correction handling;
4. `github_index.py`, when reading or publishing the per-SHA cache; and
5. `ci-hub.rs remote_ledger_records_for`, for remote-freshness conclusions.

`validate_rows.py events` and the spool readers remain raw inspection surfaces
so a malformed append-only event stays visible. Every path that promotes those
bytes into a row, a publication, a cache entry, or a freshness conclusion must
first pass the shared type.

`ledger.py` and its 38 invariant tests landed first, in
`ci-hub/ledger: deterministic global union and green/timeline/bisect queries`.
This document and `publisher.py` were added on top of it.

The shared Rust type owns the complete envelope and its closed values.
`validate_event` adds storage-path and cross-event lint, while `fold` remains
the replay reader; those responsibilities are deliberately separate. The
linter refuses an enrichment that overwrites a set value, but the reader
applies whatever is in the log, in order, without re-litigating it. A reader
that enforced the lint could not replay history that was already written — and
the real corpus contains an eleven-deep enrichment chain.

## Events, not rows

Committed bytes are never edited. A later fact about an earlier run is a new
event referencing it: `run.enrich` supplies something absent, `run.correct`
changes something wrong. So a correction never destroys what was believed at
the time — which is what makes `timeline()` meaningful, and what makes a
rewritten or deleted line a *detectable violation* rather than an ordinary
update.

## Live producer and reader boundary

Hermit and Reverie send each completed HistoryRow to `validate_rows.py record`.
It converts the row to the versioned event envelope, redacts workspace-local
paths, and atomically makes one immutable batch visible under
`ignored/ci-hub/validate-ledger-spool/`. Every ci-hub consumer calls
`validate_rows.py rows` (directly or through the Rust front controller), which
unions that live spool with every tracked shard. The same event therefore stays
visible before, during, and after publication; `event_id` deduplication reduces
the spool and tracked copies to one logical event.

`ledger-recovery/<team>/<host>/<month>/<source-commit>.jsonl` is a narrowly
scoped migration-preservation shard, read only through that same union. It is
created only when two already-published append-only replicas have divergent
line order and therefore cannot become byte-identical by appending. The split
command copies the previous remote shard there byte-for-byte in the same commit
that makes the primary match the untouched live source; it refuses unless all
source events already exist remotely with identical bodies and the event-id
union is unchanged. Normal producers and publishers never write recovery
shards.

This is one ledger boundary, not two selectable ledgers. Consumers may not read
the spool or tracked shards independently, discover another parent, or select a
receipt path through the environment. The ignored spool is unpublished live
state; the tracked shards are append-only published history; `validate_rows.py`
is the only readable union. Test fixtures may redirect the parent only behind
their explicit test-mode guard.

Acceptance-test rows are not validation observations and are excluded from the
canonical event and HistoryRow views. Historical
`ledger-live-sync-acceptance` events remain byte-for-byte in the append-only
tracked shard, but `validate_rows.py` does not serve them to authority consumers
and refuses that producer at the production write boundary. An isolated test
root may retain fixture events for its own assertions without making them
canonical validation rows.

Publication must run from a dedicated clean clone or slot. The operational
driver refuses to use the shared producer parent, creates a commit with the
required attribution, and delegates the push to `scripts/parent-main-write` so
it shares the machine-wide serialization and fresh-origin CAS boundary.

### Synchronous cleanup-publication identity

`validation_cleanup_publication.publish_validation_cleanup_proof` is the
narrower boundary for a data-lifecycle decision that requires independent
publication, rather than ordinary local visibility. It accepts only the fixed
metadata-only `run.annotate` shape, spools it through the existing publisher,
fresh-fetches `origin/main`, and returns a frozen
`ValidationCleanupPublication` bound to the repository roots, producing
checkout and exact source HEAD, shard path, exact event bytes, introduction
commit, and observed remote-tip blob. The production default runs both
publishing and fetches through `with-proxy`; isolated tests substitute a local
pass-through wrapper.

Persist `publication.to_dict()` with the run record. Immediately before the
guarded lifecycle action, recover it with
`ValidationCleanupPublication.from_dict(...)` and call
`recheck_validation_cleanup_proof` while still holding the action's lock. Use
the refreshed identity it returns. The recheck reads only the freshly fetched
remote ref and refuses a wrong repository, source checkout, shard, event,
changed bytes, missing publication ancestry, or a non-fast-forward/stale prior
observation. Pending and retained spool batches are intentionally outside this
proof boundary and cannot satisfy it. Every publisher and proof Git subprocess
uses the canonical repository-selector environment sanitizer and then disables
replacement-object resolution, so ambient `GIT_DIR`-class variables or a local
`refs/replace` entry cannot substitute an invented repository or tree for the
commit named by the fetched remote. Shards are read as bytes and decoded as
strict UTF-8 through a typed refusal; replacement decoding never changes the
bytes being attested.

## Publishing

```sh
# One-time/non-destructive migration: copy tracked rows into the same spool.
python3 ci-hub/ledger/publisher.py stage-tracked \
  --source-root "$PWD" \
  --spool-dir "$PWD/ignored/ci-hub/validate-ledger-spool"

# Normal publication. REPO must be a dedicated clean checkout, never $PWD.
python3 ci-hub/ledger/publisher.py publish \
  --source-root "$PWD" \
  --repo /path/to/dedicated/dev-hermit-publisher \
  --spool-dir "$PWD/ignored/ci-hub/validate-ledger-spool" \
  --writer "$PWD/scripts/parent-main-write" \
  --attribution '[impl agent, CODEX] [ledger-publisher, devbig014]'

# Migration-only, after normal publication proves the remote contains every
# source event but line ordering prevents byte identity. The source is read-only.
python3 ci-hub/ledger/publisher.py split-published-shard \
  --source-root "$PWD" \
  --repo /path/to/dedicated/dev-hermit-publisher \
  --shard ledger/hermit/devbig014/2026-08.jsonl \
  --writer "$PWD/scripts/parent-main-write" \
  --attribution '[impl agent, CODEX] [ledger-publisher, devbig014]'
```

Three properties, in the order they matter:

1. **A local event is never lost.** Events hit an immutable spool batch before
   any Git operation. A publisher snapshots batch filenames and drops only
   those files once their commit is confirmed to be an *ancestor of the
   freshly-fetched branch*. A batch created by a producer while publication is
   active is outside that snapshot and survives for the next pass. A push exit
   code alone is not confirmation.
2. **A concurrent publisher never costs a row.** A rejected push is retried
   with an **append-aware merge** — re-read the remote shard, keep every remote
   line in place, re-append only what is not already there. Never
   `checkout --theirs`, never a whole-file replacement.
   Publishers sharing one parent checkout also serialize its complete mutation
   transaction, including fresh fetch, checkout, candidate construction, guarded
   push, readback and spool retention. The migration-only split uses the same
   lock in that checkout's Git directory. Acquisition waits at most 60 seconds;
   timeout refuses without draining pending events. The descriptor closes on
   exec and is released on success or failure.
   Native checker preparation runs before this checkout lock. Real event
   validation still runs inside the transaction using the same source-bound
   checker. The checkout lock precedes the existing machine-wide
   `parent-main-write` lock; both are released before cell-ledger publication.
   The cell ledger retains its existing separate writer lock. This does not
   widen retries or relax the freshly fetched main ancestry requirement.
3. **Normal publication never rewrites a row.** `verify_append_only` runs
   against the remote content before each push; a non-extension is refused.
   The migration-only split is distinct: it retains the prior shard bytes
   verbatim in a recovery shard and proves the logical union is unchanged.

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
