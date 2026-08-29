# Landing mutex (`ci-hub land-lock`)

**Follow skill `hermit-validation-authority` for validation authority.**

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
| `reclaim-dead` | release only when the supervised lock-holder process is proven gone |
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
the lease as `QUARANTINED`, not merely process-exited, and every ordinary acquire,
renew, release, and exited-holder reclaim refuses. While recorded identities are
live, even explicit recovery is refused. A complete residual record becomes
recoverable only after every exact PID/start-ticks identity and its group are
absent; `reclaim-dead` must additionally prove the lock-holder process exited. A
same-boot `published`/`census-pending` record with no final census remains
unrecoverable even after its leader disappears, because an escaped descendant
may be unrecorded. A host reboot (different boot ID) is the stronger proof that
such a process domain is gone. This preserves the rule that one lander never
force-releases another lander's payload.

The owner sidecar reports `lock_holder_process=alive`, `exited:...`, or
`unknown:...`. The field describes the exact PID/boot/start-ticks process that
held the landing lock; it does not describe the liveness of the named agent. A
supervised lease whose lock-holder process has exited with **no** cleanup
authority is shown as `LOCK-HOLDER PROCESS EXITED (reclaimable)` for
backward-compatible/manual cases.

The holder file format is byte-compatible with pre-sidecar landers. A legacy
bare `acquire` has no process evidence and therefore remains lease-only: it is
never declared dead merely because a PID was not recorded.

## Shared landers (`land-pr.sh`, `union-rebase.sh`)

The land sequence itself lives here too, not only in `scratch/`:

| script | role |
| --- | --- |
| `ci-hub/landing/land-pr.sh <PR> <BRANCH> [--repo rrnewton/hermit\|rrnewton/reverie] --checkout <reviewer-repo-slot> [--union\|--no-rebase]` | reviewer-owned, detached-by-default single-PR lander: validates that the checkout origin matches the selected repository, self-wraps in `land-lock run --child-deadline`, follows that repository's validation authority, requires applicable exact-review evidence, performs a head-matched merge, and proves ancestry. The repository defaults to `rrnewton/hermit`; `--union` remains Hermit-only. |
| `ci-hub/landing/union-rebase.sh <hermit-wt> <BRANCH> [--push]` | authoritative additive union-rebase of the shared manifest registries (`*.toml` by `[[test]]` id, `test-files.json` by path, `matrix.tsv` by row); the derived `ci/expected-e2e-plan.json` is regenerated, never hand-unioned |

### Reviewer lands or bounces

The implementer runs VALIDATE and hands the conceptual landing responsibility
to the reviewer. An approving reviewer invokes this lander from their own
registered Hermit slot; a rejecting reviewer returns the change to the
implementer. There is no separate activator handoff or token artifact. The
reviewer supplies their process-resolved identity for comments. Review authority
must bind the current head or a patch-equivalent pre-rebase head proven by an
empty range-diff; LEDGER authority remains the validation predicate.

`collect_outer -> verify_outer -> enter_land_lock -> verify_final -> normal_non_admin_merge -> refresh_main -> prove_ancestry -> read_main_gate_blob -> patch_registration -> verify_registration`

Putting refresh and ancestry proof between merge and registration makes
"register only after the push" mechanical rather than remembered.

Parent `AGENTS.md` is the publication authority. Normal work uses reviewed PRs;
Red-Tip and the nine-condition rotation above are the only named direct-main
scopes. Hermit `docs/MERGE_QUEUE.md` describes mechanisms; its broad direct-FF
sentence grants no permission.

Remediation follows skill `hermit-validation-authority` for validation verdicts. No
validation result by itself authorizes a revert; that needs a durable owner
record bound to the exact candidate revert, current-main, and failed-land
identities.

### Parent changes do not manufacture Hermit validation

The parent has no Hermit VALIDATE producer and must not append synthetic rows to
the Hermit LEDGER. Follow skill `hermit-validation-authority` for the current hosted boundary
and owner-bootstrap rule.

`land-pr.sh` applies these race-tolerance rules after selecting validation
authority through skill `hermit-validation-authority`:

1. **Resolve validation state first** through the canonical policy and its
   named evidence sources.
2. **The merge command is the mergeability arbiter** — do not gate on
   `mergeStateStatus` (it sticks at `UNKNOWN`). Attempt `gh pr merge --rebase`
   once under the freshly evaluated final-boundary verdict. A refusal restarts
   the lander from its authority checks so mutable-tip evidence cannot age
   inside a merge retry loop.
3. **Treat labels and hosted observations according to `hermit-validation-authority`;** this
   README does not redefine their evidentiary value.
4. **Dereference final authorization** immediately before merge. The merge
   itself uses `--match-head-commit` so a concurrent push cannot inherit that
   authorization. A failed merge call does not reuse this verdict: the next
   attempt starts at the gate and re-fetches both bases.

### `--no-rebase`: avoid an unnecessary rewrite

**Opt-in; the default path is unchanged, and it is mutually exclusive with
`--union` (which is itself a rewrite driver).** It avoids rewriting an already
usable head. A rebase does not automatically destroy all validation knowledge:
hard-green and soft-green classification follows skill `hermit-validation-authority`.

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
initial and final authority, the merge-gate diagnostic observation, receipt
dereference, `--match-head-commit`, and the post-merge `mergeCommit.oid`
ancestry proof all still run. Whether the candidate's
validation is landable after `main` moves follows skill `hermit-validation-authority`.
Prefer the default rebase when the branch genuinely needs to be updated. Compare
`rrnewton/hermit#1812`: an unconditional
rebase-and-force-push in the union driver amended main's tip onto two PR
branches and landed #1188/#1209 as semantic no-ops.

### Historical hosted deployment plan

The superseded hosted deployment plan is intentionally not repeated here.
Current policy and the dated history of that path live in the final section of
skill `hermit-validation-authority`.

Every terminal bail emits a visible ABANDON signal on stderr and attempts a
role-tagged PR comment through the registered GitHub boundary.  If GitHub does
not accept that write, the lander preserves the real error in a JSON file under
`${CI_HUB_LANDING_LOG_DIR:-ignored/ci-hub/landing}` and prints the exact file
path loudly in the retained landing log.  Enumerate those failures with:

```bash
find ignored/ci-hub/landing -name '*-comment-failed.json' -print
```

Before this behavior was added, comment failures were reduced to
`WARN: could not post ABANDON comment` and could disappear with an agent's
terminal.  Those earlier failures cannot be reconstructed from pull requests,
so no historical count is claimed.  Before taking the lock, the shared lander
persists a post-land intent. Only after the merged SHA is ancestry-confirmed
does it arm concurrent exact-SHA local and GitHub verification; ORC recovery
closes the merge-before-arm crash window.

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

The legacy `--gate-deadline` name remains a compatibility input used to size the
default whole-child ceiling. It no longer bounds a GH Actions wait: the lander
observes merge-gate state once and proceeds from the selected authority.

Every wait in one landing attempt has an explicit scope:

| wait | bound | scope |
| --- | ---: | --- |
| lock acquisition | 1800s | FIFO wait before this landing owns the lock |
| lock lease | 900s, renewed every 300s | dead-holder safety; not a land duration |
| compatibility bound | 1080s by default | sizes the default whole-child deadline; does not wait on GH Actions |
| whole child | 2160s by default | entire lock-held subtree; `land-pr.sh` derives twice an overridden compatibility bound unless explicitly set |
| merge attempt | one | a refusal restarts from fresh authority checks |
| label / ready settling | 4s each | fixed sleeps before polling |
| termination grace | 5s | SIGTERM-to-SIGKILL interval after child timeout |

Individual `git` and `gh` calls do not have separate per-call timers. They are
inside the whole-child process-group deadline, so a stalled call cannot hold the
landing lock beyond that ceiling. The child deadline must be greater than the
compatibility bound; zero is rejected rather than meaning unbounded.

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

**Do not poll `mergeStateStatus` to decide you've landed.** The tracked lander
attempts the merge after validation qualifies under `hermit-validation-authority`; it never
uses `mergeStateStatus` as authority. After merge, that field may read `UNKNOWN` —
check `state == MERGED` and `mergeCommit`, not the merge-state. Also note the merged SHA is the
**rebased** commit (a fresh 40-hex), not your pre-merge branch head.

---

# Rebase wrapper + soft-green query (`rebase_wrapper.py`)

`ci-hub/landing/rebase_wrapper.py` records the provenance tuple
`revision X rebased on main Y -> Z`, conflict details, resolver judgement, base
floors, and evidence references. **Follow skill `hermit-validation-authority` for the sole
definitions of hard green, soft green, and landability.** Legacy
`receipt_at_Z` and `landable` fields are implementation caches and must not be
read as an independent policy that invalidates preserved X-to-Y evidence.

**Record schema** (`ignored/rebase-records.jsonl`, append-only, latest-per-Z):
`{ schema_version, recorded_utc, source_rev:X, base:Y, result:Z, conflicts:[]|
[files], resolver, risk_judgement, rationale, soft_green, base_clears_floor,
base_unmet_floors, receipt_at_Z: null|{sha:Z, verdict:VALIDATED, profile,
selection_mode, finished_at, slot, host, qualifying_count}, landable,
landable_reason }`.

## Consumer contract (the lander QUERIES; it does not read notes)

```
ci-hub/landing/rebase_wrapper.py authorize --result <Z> --current-base <Y>
ci-hub/landing/rebase_wrapper.py eligible                 # operational cache view
```

`authorize` is the single lander consumer. It first delegates hard-green to the
exact-head authority. For soft-green it treats the append-only row as untrusted
cache input, re-dereferences source X through `validate-status`, and replays X on
fresh current base Y in an isolated clone; the conflict-free recomputed tree must
equal Z. `eligible` is only an operational view over the cache and is not landing
authority. Producer paths:

```
# mechanical: ci-hub owns the rebase, auto-soft-greens the zero-conflict case
rebase_wrapper.py rebase --source <X> --onto newest-green [--push]

# resolver: after resolving conflicts, record with the mandatory judgement
rebase_wrapper.py record --source <X> --base <Y> --result <Z> \
    --conflicts <files> --resolver <agent> \
    --risk-judgement retained-soft-green|needs-full-validate --rationale "..."
```

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

`eligible` list mode reconciles against the live open-PR population and reports
the wrapper's stored buckets. Interpret those fields only through
`hermit-validation-authority`; absence from one bucket is not proof of non-landability.

**Cross-host durability (default).** A machine-local JSONL cannot carry the one
non-derivable datum (the soft-green level + resolver judgement + rationale) to
another host. `record`/`rebase` publishes exactly that datum by default,
content-addressed and keyed by Z, to the shared `validation-receipts` branch and
stores the resulting path, digest, and branch commit in the local JSONL row;
`eligible --durable-provenance` recovers an otherwise-`unaccounted` open-PR head by
dereferencing it. Durability supplies provenance, never landability. Network stays off
the `eligible` hot path. `--no-publish-provenance` is an explicit offline escape;
it leaves the record box-local and must not be used by a normal landing flow.

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
python3 ci-hub/landing/preflight.py --ledger-event '<run-result.json>' \
  --receipt-sha '<validated-sha>'                                         # 2: nonzero executed tests?
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
2. **Validation qualification follows skill `hermit-validation-authority`.** This preflight
   delegates the nonzero count decision to the Rust reader of the typed ledger
   event and Hermit's shared `HistoryRow`. Missing, malformed, wrong-commit,
   zero, and negative typed rows do not pass; human log text is never a
   fallback. It does not define green or landability independently.
3. **Landing is verified by `mergeCommit.oid` ancestry on a freshly-fetched
   remote** — never the PR head (always false after a rebase replay: that read
   79 unlanded when 46 had landed), never the `MERGED` flag alone (a later
   force-push orphans the replay SHA).

Negative tests proving each check refuses its real bad case:
`python3 -m pytest ci-hub/landing/test_preflight.py -q`
