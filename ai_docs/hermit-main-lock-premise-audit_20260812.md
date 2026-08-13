# Is the devbig014 lock on hermit main live or stale? Neither — it does not exist

2026-08-12. Task `verify-devbig014-main-lock-is-live-not-stale`. Read-only audit. No lock
state was read destructively, forced, stolen, expired, or modified; nothing was pushed to
`rrnewton/hermit`.

## Answer

**The premise is refuted.** There is no lock held by `devbig014` on `rrnewton/hermit:main`,
so it is neither live nor stale. Every mechanism that could constitute one was checked and
either does not exist, is scoped to a single box, or reports free.

The queue is real — 26 open pull requests, none merged for about nine hours — but what gates
it is **review authority**, not a lock. `reviewDecision` is `NONE` on all 26.

The likely origin of the phrasing is a real fact with the wrong shape: `devbig014` is
currently the **only box that can mint a full validation receipt**, because `devbig030`'s
kernel lacks CPUID faulting
([measured today](host-capability-cpuid-faulting_20260812.md)). A capability monopoly is not
a lock, and treating it as one prescribes the wrong action — waiting for a release that will
never come, instead of routing a validate to `devbig014` or obtaining a review.

## Mechanisms checked

### 1. `ci-hub land-lock` is per-box by construction

It cannot express a cross-machine hold. Two independent scopings:

- The lease is `repository_lock_root(root)/.landing-lock`
  (`ci-hub/lib/landing_lock.rs:3279`), resolved to the git **main worktree of the local
  clone**. Each machine has its own clone, hence its own lease file.
- The exclusion anchor is `box_exclusion_anchor_path()` =
  `/run/user/<uid>/ci-hub/landing-box.lock` (`:3307`), documented at `:3318` as
  "Box-global exclusion anchor: ONE per box, per uid, for every repository."

`foreign_box_claim` (`:2585`) reinforces the scope rather than widening it: it enumerates the
**local** claims directory, so "foreign box" there means another worktree on the same
machine, not another machine.

### 2. This box's landing lock is free

```
$ ./ci-hub/ci-hub land-lock status
FREE
```

Corroborating file state: no `.landing-lock` and no `.landing-lock.owner` exist; only
`.landing-lock.guard` (Aug 5) and `.landing-lock.queue` (Aug 12 10:35) remain. The anchor
records `pid=968286 host=devbig030`, and **that PID is dead** — a stale anchor record whose
owner exited, while `status` still correctly reports `FREE`. That is a stale artifact of the
kind this task was told to expect, but it is a devbig030 record naming a devbig030 PID, and
it is not blocking anything.

### 3. GitHub has no lock either

```
$ gh api repos/rrnewton/hermit/branches/main/protection
{"message":"Branch not protected", "status":"404"}
```

No branch protection, no push restriction, no required reviewers. Nothing server-side
serializes landings or reserves `main` for one machine.

### 4. The directive ledger records obligations, not locks

`ci-hub/directives/ledger.json` carries two `gate` entries — the `dbi`→`dbt` rename (gated on
the PR queue draining) and the fail-closed unsupported-syscall default (gated on an owner
decision). Neither names `main`, a machine, or a hold.

### 5. Nothing in the task graph claims it

`tg list` contains **zero** tasks mentioning `devbig014` and **zero** mentioning a lock or a
hold on main. The only "main lock" strings in the repository refer to
`scripts/parent-main-write`, the serialized writer for the **dev-hermit parent** repository —
a different repository and a different mechanism, which I used myself three times today
without contention.

So the claim has no artifact behind it. It appears in dispatch prose only.

## devbig014 is alive — which is a separate question

Liveness of the machine was established, and it is not evidence of a hold.

- Parent commit `98e076e8` at **2026-08-12 19:24:34 PDT** carries
  `[ledger-publisher, devbig014]` — roughly 28 minutes before this check.
- That machine token is **authenticated, not self-declared**:
  `scripts/check_commit_attribution.py` derives it from `socket.gethostname()`
  (`detect_machine`, line 60) and refuses a mismatch (line 189). Absent the documented
  `HERMIT_COMMIT_MACHINE` test override, a commit tagged `devbig014` was produced on
  devbig014.
- Its last **published** validate is `2026-08-12T20:16:34Z` (13:16 PDT), a failing run on
  `03a082dc`; 137 of 137 `privileged-cpuid.faulting` gates on that box have passed.

Limits of this evidence, stated rather than glossed: I could **not** probe devbig014 directly
— `ssh devbig014` returns `Permission denied (publickey,password)` for this account — so I
cannot enumerate its processes or read its local lease. That does not weaken the conclusion,
because by mechanism 1 a lease held there could not block this box regardless of its state.

## What is actually gating the queue

| measurement | value |
| --- | --- |
| open PRs on `rrnewton/hermit` | 26 |
| `mergeable` | 26 of 26 `MERGEABLE` |
| not draft | 17 |
| `reviewDecision == APPROVED` | **0 of 26** (all `NONE`) |
| last PR merged | `2026-08-12T16:38:02Z` (09:38 PDT), #2187 |
| `main` tip | `1f4e430c1`, 2026-08-12 10:33:26 PDT — a direct commit, later than the last PR merge |

Review, not mergeability, is the universal blocker. That matches the earlier drain-gate
analysis in `ai_docs/transient/2026-08-12-hermit-drain-gate-analysis.md`.

The four PRs the brief names as queued behind "the lock" are not blocked by anything external
to themselves:

| PR | draft | labels | last update |
| --- | --- | --- | --- |
| #2215 ci: publish manifest metadata before consumers | no | **none** | 19:51 PDT |
| #2217 Make KVM L2 evidence comparable | no | **none** | 19:44 PDT |
| #2218 docs: separate validation from merge authorization | no | **none** | 19:30 PDT |
| #2221 ci: isolate lsof mount-table input | **yes, draft** | none | 19:39 PDT |

No `passed-review-*`, no `locally-validated`, no `post-facto-human-review`. The rest of the
queue does carry those labels (`passed-review-claude` 3, `passed-review-codex` 3,
`post-facto-human-review` 7, `locally-validated` 2), so their absence here is a real
difference, not a repository that has no labels. And #2217 has no status checks at all
(`statusCheckRollup` empty), consistent with hosted CI being off.

All four were updated between 19:30 and 19:51 PDT — agents are actively working them right
now. They are in progress, not held.

## What an agent should take from this

- **Do not wait for a release.** There is nothing to release. An agent whose PR is not
  landing should look at its review state and its receipt, in that order.
- **Do not "break" or clear anything on the strength of this report.** Nothing needs
  breaking; that is the point. The stale devbig030 anchor record is inert and `status` already
  reports `FREE` despite it.
- **Do not merge the three constraints.** Review authority (this note), the CPUID-faulting
  receipt capability
  ([host-capability note](host-capability-cpuid-faulting_20260812.md)), and box
  load are independent. Only the second one is machine-specific, and its remedy is routing to
  devbig014, not waiting.

## Not established

- devbig014's local landing-lock state, process list, or what it is working on — no SSH
  access from this account.
- Whether any human or coordinator ever *intended* a convention that one box lands at a time.
  If such a convention exists it is unwritten: it has no artifact in ci-hub, the directive
  ledger, the task graph, GitHub, or either AGENTS.md.
- Why `main`'s tip is a direct commit rather than a PR merge; I observed it and did not
  investigate the path it took.

## Commands used

```sh
./ci-hub/ci-hub land-lock status
ls -la .landing-lock* /run/user/$(id -u)/ci-hub/
cat /run/user/$(id -u)/ci-hub/landing-box.lock; [ -d /proc/968286 ] || echo dead
grep -n 'fn repository_lock_root\|fn box_exclusion_anchor_path\|fn foreign_box_claim' \
  ci-hub/lib/landing_lock.rs
with-proxy env FLEET_REAL_GH=$(command -v gh) ./ci-hub/bin/gh api \
  repos/rrnewton/hermit/branches/main/protection
with-proxy env FLEET_REAL_GH=$(command -v gh) ./ci-hub/bin/gh pr list --repo rrnewton/hermit \
  --state open --limit 100 --json number,mergeable,isDraft,reviewDecision,labels
git log origin/main --format='%H %ad' --date=iso   # then read each trailer for its machine
grep -n 'def detect_machine' -A18 scripts/check_commit_attribution.py
tg list | grep -icE 'devbig014|lock'
```

Note for future GitHub queries from an agent session: `./ci-hub/bin/gh` needs
`FLEET_REAL_GH` set, and the API is only reachable through `with-proxy`. Plain `gh` fails
with `dial tcp … connect: network is unreachable` even though `git` push/fetch to github.com
succeeds, because git carries its own proxy configuration.
