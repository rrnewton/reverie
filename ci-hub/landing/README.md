# Landing mutex (`ci-hub land-lock`)

A small, deterministic **shared-file landing mutex** that serializes PR landings
which mutate the same shared manifest registries. Discoverable, not folklore.

## Why it exists

Every backend-parity / e2e-manifest PR mutates the **same two** shared files:

- `hermit/tests/e2e/manifests/backend-parity-c.toml`
- `hermit/tests/e2e/manifests/inventory/test-files.json`

When several landers push + merge concurrently, **each land moves `origin/main`
and DIRTYs every other in-flight PR**, so the pack "serializes" the hard way —
by collision-and-retry, burning the single self-hosted `[gate]` runner and
converging slowly (the documented "mass-parallel drain won't self-heal;
SERIALIZE" failure mode). This mutex turns that scrum into an orderly queue:
**exactly one land is in flight at a time.**

## Contract

Every lander MUST hold the lock around its entire land sequence:

```
acquire  ->  re-union onto fresh origin/main  ->  push  ->  stamp
         ->  merge --rebase  ->  ancestry-verify  ->  release
```

Acquire **before** fetching fresh main (so your re-union sees the final state of
the previous land); release **only after** `git merge-base --is-ancestor <sha>
origin/main` confirms your commit actually landed.

## Design (small + deterministic)

- Typed Rust command variants in `ci-hub/ci-hub.rs` own acquire, renew,
  release, status, and run. `landing-lock.sh` is an exec-only compatibility
  path for landers and heartbeats that started before the Rust cutover.
- An advisory **`flock`** on the guard file makes each check-and-set atomic
  across old and new processes.
- The held state is a **lease with an expiry**, not a held fd — so acquire in one
  shell and release in another Just Work, and **(a) a dead holder cannot wedge
  the pack**. Supervised `run` leases record host, boot ID, PID, and process
  start time in a sidecar. A waiter can reclaim immediately when that exact
  process is proven gone; legacy/manual leases remain protected until their
  lease lapses (`--hold` seconds, default 900).
- **(b)** The lockfile records holder **agent + PR + host + timestamps** for
  debuggability; `status` prints them.
- **(c)** Waiters enqueue in a **FIFO**, so ordering is deterministic and each
  waiter sees who is ahead of it; `release` frees the lock immediately and names
  the next agent, which then acquires on its next short (3s) poll rather than
  polling blindly.

Runtime state (all machine-local, gitignored):

| file | role |
| --- | --- |
| `~/work/dev-hermit/.landing-lock`        | holder metadata — the lock |
| `~/work/dev-hermit/.landing-lock.owner`  | supervised owner identity; does not alter the legacy holder format |
| `~/work/dev-hermit/.landing-lock.guard`  | `flock` target (impl detail) |
| `~/work/dev-hermit/.landing-lock.queue`  | FIFO waiter list |
| `~/work/dev-hermit/.landing-lock.cleanup-required` | fsynced armed/published/residual process-domain authority; blocks ordinary acquisition and reclaim |
| `~/work/dev-hermit/.landing-lock.cleanup-required.tmp-*` | atomic-replacement scratch, machine-local and ignored |

`validate-lock` uses the identical cleanup-authority suffixes beside
`.validate-lock`; all four cleanup files are root-anchored in `.gitignore`.

## Before staging a coalesce wave: screen the constituents

A wave is selected by "ready and conflict-free". Neither property says whether a
change was **deliberately refused**, and the constituent list is written by hand
into the wave body, so a closed-on-the-merits change folded into a staging branch
lands under the wave's single approval. Run this **before** building the staging
branch, while dropping a constituent is still free:

```bash
python3 ci-hub/landing/coalesce_guard.py --repo rrnewton/hermit \
    --gh-cmd "with-proxy gh" --wrap "agent-utils/bin/herdr-run --agent <you>" \
    1622 1549 1470 ...          # the exact constituent list
# exit 0 = every constituent may be folded
# exit 3 = REFUSED; the output names each refused PR and why
```

`--wrap` is needed only on the sandbox egress path; plain `--gh-cmd "with-proxy gh"`
works where `gh` can reach GitHub directly.

**It is fail-closed, and that is the point.** Three outcomes:

| verdict | meaning | what to do |
| --- | --- | --- |
| `ALLOW_SUPERSEDED` | the close names a successor that carries the work | fold it |
| `REFUSE_MERITS` | an owner decided it must not land | **drop it from the wave** |
| `REFUSE_UNKNOWN` | the close is unannotated | read the close, annotate, re-run |

**A `REFUSE_UNKNOWN` is not a bug and not a reason to disable the guard.** It
means nobody has yet recorded whether that close was a supersession or a
rejection. The fix is one row in
[`closed_pr_dispositions.tsv`](closed_pr_dispositions.tsv) naming the successor
or the reason — then re-run. The 45 unannotated closes outstanding on 2026-08-07
have already been classified, so a 46th appearing is the *normal* steady-state
cost of a new close, not an outage. Disabling the guard to get past one converts
a one-line annotation into a silent re-land.

Why fail closed at all: wrongly refusing costs a sentence naming the successor;
wrongly allowing re-lands work an owner spent review effort rejecting, inside a
batch whose single approval hides it.

**This is prevention, not remediation.** PR #1633 was cited as an occurrence and
is **not** one — all 23 of its constituents were closed 3–5 minutes *after* it
merged, each with "LANDED via coalesce batch-2". The exposure is real; there is
no confirmed instance of it firing.

Scope: the guard proves *an owner pointed somewhere else*, **not** *the work is
on main*. Ancestry verification owns that second claim; do not read an
`ALLOW_SUPERSEDED` as proof the successor landed.

## Usage

```bash
cd ~/work/dev-hermit

# Inspect
ci-hub/ci-hub land-lock status

# Canonical land: detached by default; prints the durable log path and PID
ci-hub/landing/land-pr.sh 1533 codex/my-branch

# Manual acquire / release around your land sequence
ci-hub/ci-hub land-lock acquire --agent hermit-ci --pr 1533   # blocks until yours
#   ... re-union -> push -> stamp -> merge --rebase -> ancestry-verify ...
ci-hub/ci-hub land-lock release --agent hermit-ci

# Crash-contained wrapper (RECOMMENDED): acquire, run, and release only after
# proving the payload domain empty. A background heartbeat renews the lease so
# a long land keeps the lock; a HARD --child-deadline kills a wedged subtree,
# then releases on complete cleanup proof or retains a quarantine otherwise.
ci-hub/ci-hub land-lock run --agent hermit-ci --pr 1533 \
  --child-deadline 2160 -- ./my-land-sequence.sh
```

### Subcommands

| command | purpose |
| --- | --- |
| `acquire --agent NAME --pr N [--wait S] [--hold S]` | block until acquired (FIFO); reclaims a lapsed lease |
| `renew --agent NAME [--hold S]` | heartbeat — extend your lease during a long land |
| `release --agent NAME` | free the lock (owner only); signals the next waiter |
| `status` | print holder metadata, process liveness, seconds left, and the FIFO queue |
| `reclaim-dead` | release only when the supervised owner process is proven gone |
| `run --agent NAME --pr N [--child-deadline S] [...] -- CMD...` | acquire → run CMD (auto-heartbeat, hard child-deadline) → release after complete cleanup proof, otherwise quarantine |

Defaults: `--wait 1800` (give up after 30 min), `--hold 900` (lease lapses after
15 min so a dead holder self-clears), `--child-deadline 2160` (kill a wedged
land subtree after 36 min). Poll interval 3s.

Exit codes: `0` ok · `1` wait-timeout · `2` usage · `3` not-owner / internal ·
`124` child-deadline breach with the land subtree proven gone and the lock
released. An incomplete cleanup proof instead returns an error and retains a
`QUARANTINED` lock.

### `--child-deadline`: no unbounded wait may hold the queue

`run`'s heartbeat renews the lease **for as long as the child lives** — so a land
script that hangs at a failing/UNKNOWN gate would renew forever and **wedge the
FIFO permanently** (the observed ~2040-minute head-of-line starvation). The fix:
`run` supervises the child against `--child-deadline`; on breach it SIGTERMs the
whole child process group (then SIGKILL after a short grace), and performs a
complete descendant census. Only a proven-empty payload domain **releases the
lock**, prints a loud `ABANDON PR #N` line, and exits `124`; an incomplete or
nonempty census retains a `QUARANTINED` lock and returns an error. The PR is left
open for retry. A zero deadline is rejected: an unbounded wait is unboxed
compute and every wait here is bounded. `land-pr.sh`'s surviving outer
supervisor also posts a durable PR comment when the killed inner process cannot
do so itself.

**Never hand-roll a renewer.** A bare `acquire` plus an external `renewer.sh`
loop that outlives a dead agent defeats the lease-lapse safety net and is exactly
what produced the zombie-held lock. Always land under `land-lock run` (directly,
or via `land-pr.sh`, which self-wraps) so the lease is bound to a bounded child.

### Abnormal termination and evidence-based recovery

Before spawn, `run` fsyncs an `armed` cleanup authority. The gated child cannot
exec the payload until an atomic replacement publishes its exact PID/start-time
identity and process group. Before any descendant census, `run` persists
`census-pending`, disables heartbeat renewal, and joins the heartbeat; only then
may it freeze descendants and publish a complete `residual` census. Normal exit,
nonzero exit, and hard deadline clear the authority only after both the process
group and residual census prove the domain empty.

SIGKILL and machine loss cannot finish that census. `status` therefore reports
the lease as `QUARANTINED`, not merely `ORPHANED`, and every ordinary acquire,
renew, release, and dead-owner reclaim refuses. While recorded identities are
live, even explicit recovery is refused. A complete residual record becomes
recoverable only after every exact PID/start-ticks identity and its group are
absent; `reclaim-dead` must additionally prove the supervisor owner dead. A
same-boot `published`/`census-pending` record with no final census remains
unrecoverable even after its leader disappears, because an escaped descendant
may be unrecorded. A host reboot (different boot ID) is the stronger proof that
such a process domain is gone. This preserves the rule that one lander never
force-releases another lander's payload.

The owner sidecar still reports `owner_process=alive`, `dead:...`, or
`unknown:...`. A proven-dead supervised lease with **no** cleanup authority is
shown as `ORPHANED (reclaimable)` for backward-compatible/manual cases.

The holder file format is byte-compatible with pre-sidecar landers. A legacy
bare `acquire` has no process evidence and therefore remains lease-only: it is
never declared dead merely because a PID was not recorded.

## Shared landers (`land-pr.sh`, `union-rebase.sh`)

The land sequence itself lives here too, not only in `scratch/`:

| script | role |
| --- | --- |
| `ci-hub/landing/land-pr.sh <PR> <BRANCH> [--union\|--no-rebase]` | detached-by-default full single-PR lander: self-wraps in `land-lock run --child-deadline`, requires Hermit's counted local-full **or** versioned hosted-portable exact-head authority before and after rebase, derives `locally-validated` only for a qualifying local receipt, polls merge-gate, rechecks the same authority at the final mutation boundary, performs a head-matched rebase merge, then verifies ancestry |
| `ci-hub/landing/gate_definition_rotation.py --pr N --branch B --checkout WT --execute` | narrow self-authorization fallback: verifies an owner-authored exact head/base/blob/path record, dual exact-head reviews, counted receipt, and typed definition-only self-refusal; then composes through `land-pr.sh --gate-rotation --no-rebase` and registers `MERGE_GATE_V4_BLOB` only after mergeCommit ancestry |
| `ci-hub/landing/union-rebase.sh <hermit-wt> <BRANCH> [--push]` | authoritative additive union-rebase of the shared manifest registries (`*.toml` by `[[test]]` id, `test-files.json` by path, `matrix.tsv` by row); the derived `ci/expected-e2e-plan.json` is regenerated, never hand-unioned |

### Merge-gate definition rotation

The gate definition is intentionally unable to authorize a PR that replaces
its own registered blob. That typed `SELF_AUTHORIZATION_REFUSED` result is a
precondition for the narrow independent rotation path, not permission by
itself. The newest owner-authored PR comment must contain this marker on its own
line followed by one JSON object:

```text
GATE-DEFINITION-ROTATION-AUTHORIZED
{"repo":"rrnewton/hermit","pr":2071,"base":"<40-hex>","head":"<40-hex>","old_blob":"<40-hex>","new_blob":"<40-hex>","allowed_paths":[".github/workflows/merge-gate.yml"],"reason":"...","expires_at":"2026-08-10T17:00:00Z","authorized_by":"rrnewton"}
```

Trusted current-main code independently re-derives all identities, changed
paths, both `APPROVED-AT` lanes, absence of a current `REQUEST CHANGES AT`, the
counted exact-head receipt, and the definition-only gate failure. A base move,
head move, blob mismatch, extra path, malformed or expired record, stale review,
current rejection, missing receipt, or non-definition gate failure refuses.

Execution has one merge path: the normal non-admin
`gh pr merge --rebase --match-head-commit` inside `land-pr.sh`. There is no
gate-specific raw-FF primitive and no gate-specific `--_inner`. Even a direct
caller of the lander's rotation mode must prove it is a descendant of the exact
PID/start-ticks identity recorded by `land-lock run`. After the inner lander
proves the PR's `mergeCommit.oid` is on `origin/main` and the lock releases, the
normal lander's outer process verifies that main contains the authorized
workflow blob and only then updates and re-reads
`MERGE_GATE_V4_BLOB`. A failed registration leaves the gate fail-closed; retry
with `--register-only`, never weaken or roll back the gate.

### Parent changes: hosted exact-head evidence, not a synthetic validate receipt

The local validation ledger is deliberately bound to `rrnewton/hermit` because
the parent repository has no Hermit validation producer. Parent tooling therefore
uses a distinct registered authority rather than widening that ledger with rows
whose profile and coverage fields would be fictitious:

```bash
ci-hub/ci-hub hosted-status --repo rrnewton/dev-hermit --sha PARENT_HEAD
```

A parent green is the complete 4/4 exact-head set: `Parent tooling shard`,
`ci-hub bounded operations shard`, `Reject owner-specific build paths`, and
`Demo-touching commits require a green-demo attestation`. The verifier
dereferences workflow, run, job, repository, and SHA identities. A missing or
cancelled member is `NO_RESULT`, not permission to publish; a red member is red.
Focused task tests and both sides of a mutation bracket belong in the named
shards for the mechanism they protect. Text in a PR body or a hand-applied label
cannot replace this authority.

`land-pr.sh` bakes in the three race-tolerance fixes so a transient CI state
never wedges a land:

1. **Race-tolerant exact-head gate poll** — query the Actions workflow runs for
   the rebased head and evaluate the latest `pull_request` or
   `workflow_dispatch` run. The dispatch event matters: the `workflow_run`
   controller runs on `main`, and its dispatched PR-head success can be absent
   from `statusCheckRollup`. Ride through transient
   `FAILURE`/`IN_PROGRESS`/`QUEUED` states to `SUCCESS`; bounded by
   `--gate-deadline` (default 1080s), then ABANDON with the last event, run ID,
   URL, and state.
2. **The merge command is the mergeability arbiter** — do not gate on
   `mergeStateStatus` (it sticks at `UNKNOWN`). Attempt `gh pr merge --rebase`
   once under the freshly evaluated final-boundary verdict. A refusal reruns the
   gate so mutable-tip evidence cannot age inside a merge retry loop.
3. **Treat the label only as a cache** —
   `exact-head-validation-authority.sh` independently queries the counted local
   receipt and the versioned hosted job set. Either qualifying exact-head green
   is sufficient; missing/partial/stale evidence is NO_RESULT, and a genuine red
   from either path blocks. Only `apply-local-label` may materialize the optional
   local cache label. It requires a nonzero executed-test count, hashes the
   referenced log, and publishes the selected ledger row as an immutable receipt
   on `rrnewton/dev-hermit:validation-receipts` before commenting or labeling.
4. **Dereference the final authorization** — immediately before merge, query the
   same exact-head combiner. A local positive additionally dereferences the
   current PR comment through `ci-hub/validation/verify_receipt.sh`; a hosted
   positive remains independently sufficient. Missing, forged, stale, tampered,
   zero-executed, partial, or contradictory evidence refuses the landing before
   any merge call. The merge itself uses `--match-head-commit` so a concurrent
   push cannot inherit that authorization. A failed merge call does not reuse
   this verdict: the next attempt starts at the gate and re-fetches both bases.

### `--no-rebase`: a rebase can only downgrade an already-authorized head

**Opt-in; the default path is unchanged, and it is mutually exclusive with
`--union` (which is itself a rewrite driver).** Step 2 normally rebases the
branch onto fresh `origin/main` and force-pushes. That rewrites the head, so
step 4 re-derives the exact-head authority at a *new* SHA and every green earned
at the old SHA is orphaned — a rebase can never upgrade an authorized head, only
downgrade it to NO_RESULT.

It is also unnecessary on `rrnewton/hermit`. Re-verify rather than trust:

```bash
with-proxy gh api repos/rrnewton/hermit/rulesets --jq '.[]|"\(.id)\t\(.name)"'
```

The `main check gating (admin-bypassable)` ruleset's `required_status_checks`
rule carries `strict_required_status_checks_policy: false` (observed
2026-08-07), so main does **not** require a branch to be up to date; and
`gh pr merge --rebase` replays the commits onto the current tip server-side
anyway.

```bash
ci-hub/landing/land-pr.sh 1711 fixture/pid-tid-virtualization-identity --no-rebase
```

`--no-rebase` removes a **mutation**, never a **check**: the land-lock, the
step-1b and step-4 exact-head authority, the merge-gate poll, the final-boundary
authority plus receipt dereference, `--match-head-commit`, obligation arming,
and the post-merge `mergeCommit.oid` ancestry proof all still run. Use it when a
head already holds a qualifying exact-head green and main has since moved.
Prefer the default rebase when the branch genuinely needs to be updated (real
conflicts, or a base below a gate floor — that needs a rebase *and* a fresh
validate, not this flag). Compare `rrnewton/hermit#1812`: an unconditional
rebase-and-force-push in the union driver amended main's tip onto two PR
branches and landed #1188/#1209 as semantic no-ops.

### Deployment obligation: `hermit-merge-gate-authority-deployment`

The parent policy and lander implement portable-hosted OR counted-local
authority, but that rule is not fully deployed in Hermit until a follow-up
Hermit PR lands. Hermit's current `main` remains deliberately stricter: its
`.github/workflows/merge-gate.yml` GitHub leg requires both portable and
privileged jobs, and its two verifier-download steps pin an older dev-hermit
`verify_receipt.sh`. Therefore a portable-only hosted positive can still be
blocked by merge-gate; do not report the portable-only rule as operational in
Hermit or bypass the required check during this transition.

After this parent revision lands, the follow-up Hermit PR must:

1. update `.github/workflows/merge-gate.yml` so its GitHub positive uses the
   exact portable job set without requiring `ci-privileged.yml`;
2. advance both immutable dev-hermit `verify_receipt.sh?ref=...` downloads and
   their SHA-256 checks to this landed parent commit; and
3. update `docs/MERGE_QUEUE.md` to name the same one-job hosted authority and
   new verifier pin, then bracket portable-only positive, incomplete/no-result,
   genuine-red, and counted-local-positive paths before landing.

Every terminal bail emits a visible ABANDON signal — stderr **and** a role-tagged
PR comment — so an abandoned PR never silently languishes (the #244 pattern).
Before taking the lock, the shared lander persists a post-land intent. Only after
the merged SHA is ancestry-confirmed does it arm concurrent exact-SHA local and
GitHub verification; ORC recovery closes the merge-before-arm crash window.

```bash
cd ~/work/dev-hermit
ci-hub/landing/land-pr.sh 1470 codex/backend-parity-contract --union
# DETACHED LAND: pid=... log=.../land-pr1470-<UTC timestamp>-<pid>.log
```

The default launcher uses `nohup` plus a new session and returns immediately,
before lock acquisition. The printed timestamped log is the durable observation
surface across the agent shell's 120-second cap and agent recycling. Use
`--foreground` only for short diagnostics; it does not change any deadline.

### Deadline basis and scope

The gate default is derived from successful `demo-hot-path.yml` pull-request
runs created from `2026-08-03T23:00:00Z` through measurement on 2026-08-04:
`n=11`, min `372s`, median `586s`, p90 `646s`, p95/p99/max `864s` (nearest-rank
percentiles). The `1080s` gate deadline is `ceil(864 * 1.25)`. This is an
operational baseline for the current runner, not a claim that every future gate
finishes within it; refresh the dated sample when the runner or workflow changes.

Every wait in one landing attempt has an explicit scope:

| wait | bound | scope |
| --- | ---: | --- |
| lock acquisition | 1800s | FIFO wait before this landing owns the lock |
| lock lease | 900s, renewed every 300s | dead-holder safety; not a land duration |
| merge-gate | 1080s by default | exact-head Actions runs; caller may override |
| whole child | 2160s by default | entire lock-held subtree; `land-pr.sh` derives twice an overridden gate deadline unless explicitly set |
| merge retry | 12 attempts, 15s sleeps | at most 180s of explicit retry sleep |
| gate poll | 15s interval | included in the gate deadline |
| label / ready settling | 4s each | fixed sleeps before polling |
| termination grace | 5s | SIGTERM-to-SIGKILL interval after child timeout |

Individual `git` and `gh` calls do not have separate per-call timers. They are
inside the whole-child process-group deadline, so a stalled call cannot hold the
landing lock beyond that ceiling. The child deadline must be greater than the
gate deadline; zero is rejected rather than meaning unbounded.

### Quota-degraded drain: local patch identity first

When GitHub API quota is exhausted, stop using per-PR `gh pr view` for triage.
In an assigned slot, fetch the PR ref through Git transport (not the API) and
compare its patches locally:

```bash
git fetch origin "+refs/pull/$PR/head:refs/pr-drain/$PR"
git cherry origin/main "refs/pr-drain/$PR"
```

`-` means an equivalent patch is already on main; `+` means the patch remains
unique. Record both the fetched ref SHA and main SHA. This classification costs
zero GitHub API points. Serialize API ownership and reserve `gh` for the final
close/merge observation; parallel drainers continue local patch-id work.

## Verifying your land (before you release)

Release only after the commit is on `origin/main`:

```bash
with-proxy gh pr view PR_NUMBER -R rrnewton/hermit --json state,mergeCommit \
  -q '{state:.state, sha:.mergeCommit.oid}'          # want state=MERGED
with-proxy gh api \
  "repos/rrnewton/hermit/compare/$(with-proxy gh pr view PR_NUMBER -R rrnewton/hermit --json mergeCommit -q .mergeCommit.oid)...main" \
  --jq 'select(.status == "ahead" or .status == "identical") | "LANDED"'
```

**Do not poll `mergeStateStatus` to decide you've landed.** If auto-merge is
armed (REBASE), the PR merges the instant merge-gate goes green, and
`mergeStateStatus` then reads `UNKNOWN` for a merged PR — check `state == MERGED`
and `mergeCommit`, not the merge-state. Also note the merged SHA is the
**rebased** commit (a fresh 40-hex), not your pre-merge branch head.

---

# Rebase wrapper + soft-green query (`rebase_wrapper.py`)

`ci-hub/landing/rebase_wrapper.py` is ci-hub's **own** rebase wrapper. It owns the
whole span **rebase → push → validate-at-pushed-head → record** (not just the
rebase): it records `revision X rebased on main Y -> Z`, derives a **soft-green
confidence level**, and binds the **receipt at the pushed head Z**, so a lander
can decide whether to land on the prior or wait for a tip validate.

**Soft-green is a level, not a boolean.** Zero textual conflicts is a
high-confidence *prior*, not a proof (git conflict detection is line-based;
semantic dependency is not — a caller X added while Y changed the callee's
contract has no conflict and still breaks). So:

- `soft-green(zero-conflict)` — earned mechanically; land on the prior, **verify
  the tip post-facto, fix forward**. The two halves are one system.
- `soft-green(resolver-judged)` — a conflict was resolved and the resolving agent
  judged it low-risk enough to keep the soft green. The **risk judgement is a
  required field**: a conflicted rebase with no `--risk-judgement` + `--rationale`
  is **REFUSED (exit 2)**, never defaulted to green.

**Landability carries the base Y.** A clean rebase onto a base **below a gate
floor** yields an unlandable Z even with zero conflicts. So `landable` includes
"the base clears every floor in `validate/rebase-base-floors.json`" (delegated to
`gate_floors.py`, re-checked live at query time so a newly-added floor demotes a
stale record).

**Landability carries the receipt at the *pushed* head Z.** THE PUSH REWRITES THE
HEAD, so a receipt earned on the pre-push SHA dies with it — and the merge gate
checks the pushed Z, for which no receipt then exists. That exact gap cost an
afternoon (2026-08-04): 15 heads rebased, pushed, marked ready; **none could
merge** (`qualifying_count: 0`) because every receipt was bound to a SHA the push
had discarded. No agent was wrong — the missing step lived in the gap between the
rebase front (ends at push) and the lander (assumes a receipt). So `receipt_at_Z`
is a **first-class record field** (a `null` there is *visible*; a missing receipt
inside an assumption is not), and `eligible` **re-derives it live** by
dereferencing the one receipt authority `ci-hub validate-status --sha Z`. A
receipt appearing after recording promotes Z with **no re-record**; one revoked
demotes it. Right after `--push` the receipt is normally `null` (validate at Z has
not run yet) — the head is recorded `NOT-LANDABLE` and flips eligible only once
validate at Z completes.

**Full landability:** `landable` = soft-green **AND** base clears every live floor
**AND** a clean full-validation receipt is bound live to the pushed head Z. A push
with **no subsequent receipt is REFUSED as landable, never silently queued**.

**Record schema** (`ignored/rebase-records.jsonl`, append-only, latest-per-Z):
`{ schema_version, recorded_utc, source_rev:X, base:Y, result:Z, conflicts:[]|
[files], resolver, risk_judgement, rationale, soft_green, base_clears_floor,
base_unmet_floors, receipt_at_Z: null|{sha:Z, verdict:VALIDATED, profile,
selection_mode, finished_at, slot, host, qualifying_count}, landable,
landable_reason }`.

## Consumer contract (the lander QUERIES; it does not read notes)

```
ci-hub/landing/rebase_wrapper.py eligible                 # list landable heads
ci-hub/landing/rebase_wrapper.py eligible --result <Z>    # exit 0 eligible / 2 not
```

A query has no mailbox to miss — this closes the producer-posted-to-the-wrong-task
gap (12 verified heads were invisible that day because a producer posted to its
own task). By default `eligible` re-checks **both** the base floor and the receipt
at Z **live**; `--no-recheck-floor` / `--no-recheck-receipt` trust the frozen
record snapshot (offline). Records live beside the validate ledger at
`ignored/rebase-records.jsonl` (append-only, latest-per-Z wins). Producer paths:

```
# mechanical: ci-hub owns the rebase, auto-soft-greens the zero-conflict case
rebase_wrapper.py rebase --source <X> --onto newest-green [--push]

# resolver: after resolving conflicts, record with the mandatory judgement
rebase_wrapper.py record --source <X> --base <Y> --result <Z> \
    --conflicts <files> --resolver <agent> \
    --risk-judgement retained-soft-green|needs-full-validate --rationale "..."
```

See memory `ci-hub-ledger-cannot-record-soft-vs-hard-green` (the validate ledger
cannot record soft-vs-hard green — this store carries the provenance it can't).

## Canonical store, reconciliation, and cross-host provenance

**One store per host, never per-checkout.** The store path resolves from
`CI_HUB_REBASE_STORE` (override outright — a fleet pins one shared path), else
`ignored/rebase-records.jsonl` under `parent_root()`, which honours
`DEV_HERMIT_PARENT` and only *then* falls back to the module location. It is
**never** anchored to `__file__`: a scratch/worktree-slot copy of the wrapper has
a different `__file__`, so a `__file__`-derived path would send the producer's
records to a store the lander never reads — the same producer-wrote-own /
consumer-read-other gap `eligible` exists to kill, merely relocated onto the
filesystem. With the env set once on a host, every copy converges on one store.

**`eligible` list mode reconciles against the LIVE open-PR population** (`gh pr
list`, default `rrnewton/hermit`; `--no-reconcile` for store-only) so *invisible
!= nothing-pending*. Every open pushed PR is accounted for exactly once and lands
in a bucket: `eligible`, `pending-no-receipt` (soft-green + base OK, awaiting
validate@Z), **`receipt-unknown`** (the receipt authority was unreachable —
VISIBLE and non-landable, **never** collapsed to absent, so a validate-status
failure can no longer make an eligible head vanish silently), `unaccounted` (open
PR with no record — pushed by a path that never called the wrapper), or
`disqualified`. Recorded heads matching no open PR are reported `recorded_not_open`
(superseded / force-pushed away — the chronic push-rewrites-the-head orphan case).

**Cross-host durability (opt-in).** A machine-local JSONL cannot carry the one
non-derivable datum (the soft-green level + resolver judgement + rationale) to
another host. `record`/`rebase --publish-provenance` publishes exactly that datum,
content-addressed and keyed by Z, to the shared `validation-receipts` branch;
`eligible --durable-provenance` recovers an otherwise-`unaccounted` open-PR head by
dereferencing it — then holds it to the **same live floor + receipt gates** as a
local record (durability supplies the datum, never landability). Network stays off
the default hot path.

## Notes

- `run` is preferred over bare `acquire`/`release`: it releases on every observed
  child termination, and evidence-based recovery clears a lease if the
  supervisor itself is killed. Its heartbeat prevents a genuinely long (but
  live) land from having its lease reclaimed out from under it.
- The lease is a **safety net**, not a schedule: keep `--hold` comfortably above
  your real land time, and prefer `run` so releases happen promptly.
- Disjoint footprints don't strictly need the lock, but taking it is cheap and
  keeps the single `[gate]` runner from being contended — when in doubt, hold it.

---

# Landing preflight (`ci-hub/landing/preflight.py`)

**Run this before trusting any green.** Each check is a defect that actually
fired on 2026-08-04; the rules were previously retyped by hand into agent
dispatches, and a rule that gets retyped is a rule that decays.

```bash
# the three checks
python3 ci-hub/landing/preflight.py --sha '<handed-sha>' --pr '<n>'          # 1: SHA still head?
python3 ci-hub/landing/preflight.py --log '<validate.log>'                 # 2: nonzero executed tests?
python3 ci-hub/landing/preflight.py --landed-pr '<n>' --checkout hermit    # 3: landed by ancestry?
# the two standing traps
python3 ci-hub/landing/preflight.py --diff-of worktrees/'<slot>'/hermit    # 4: reverie patch override?
#   5: byte-identical branch -- check_no_byte_identical_branch(), library use
```

Exit **0** only when every requested check PASSes. **`UNKNOWN` blocks**: an
unanswerable check must never launder "I could not tell" into "it is fine".
Add `--no-network` to run offline (unresolvable checks then report UNKNOWN, so
the gate correctly refuses rather than passing).

1. **The SHA you were handed is a cache; the branch is the source.** Four handed
   SHAs went stale in one night.
2. **A green must carry a nonzero executed test count.** `--features` gating
   yields build-ok / target-ran / zero-executed / SUCCESS. Absent and empty logs
   are refused too.
3. **Landing is verified by `mergeCommit.oid` ancestry on a freshly-fetched
   remote** — never the PR head (always false after a rebase replay: that read
   79 unlanded when 46 had landed), never the `MERGED` flag alone (a later
   force-push orphans the replay SHA).

Negative tests proving each check refuses its real bad case:
`python3 -m pytest ci-hub/landing/test_preflight.py -q`
