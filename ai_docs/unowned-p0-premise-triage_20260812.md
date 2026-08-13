# Unowned P0 triage: which premises still hold

2026-08-12, ~21:40 PDT. Fact-finding only. **No priority was changed and nothing was closed** —
both are the owner's. `goal-hermit-v2` excluded as instructed.

## Population correction

The brief says twelve. Measured: **fifteen** tasks are P0, not closed, and unowned. Excluding
`goal-hermit-v2`, **fourteen** need triage — three more than the brief lists:

- `land-rust-validate-migration-pr1635` (BACKLOG)
- `revalidate_current_reverie_pin` (BACKLOG)
- `test_applications_e2e_stats` (IN_PROGRESS)

A second anomaly worth naming: **six of the fourteen are `IN_PROGRESS` with no owner**. An
in-progress task with no owner is an abandoned claim, not work in flight, and it reads on the
board as "someone has this" — the same information failure as an unworked P0.

```sql
SELECT local_id, status, coalesce(owner,'-') FROM tasks
 WHERE priority='P0' AND status!='CLOSED' AND (owner IS NULL OR owner='');
```

## Verdicts

| task | premise | evidence | what blocks it now |
| --- | --- | --- | --- |
| `close_open_prs_whose` | **REFUTED** (twice) | Its own note: "already-landed = ZERO of 131". Re-tested today on the current population: **0 of 27** open PR heads is an ancestor of `origin/main`. | Nothing — the work ran and returned a null result. Disposition is the owner's. |
| `full_profile_validate_returns` | **HOLDS, strongly** | **25 of 219** commits carry BOTH a passing and a failing full-profile ledger row for the *same* SHA. | Nothing blocks investigation; a fix needs validation capacity. |
| `e2e_manifests_cannot_express` | **HOLDS** | `ci/test_harness.sh` still fails a cell on nonzero exit, and grep finds **0** occurrences of `expect_nonzero`, `expected_exit`, or `allow_nonzero`. No manifest key can express it. | Nothing — implementable now, no receipt needed to write it. |
| `add_force_escape_to` | **HOLDS — and its `implemented` tag is unbacked** | The only `--force` in `scripts/check-reverie-pin.rs` is `git fetch --force` (line 477). There is no pin-lint force escape. Yet the task carries the `implemented` tag. | Part B is simply not written. The tag should not be read as done. |
| `fold_the_ci_harness` | **SATISFIED elsewhere** | PR **#2213** is open and titled "Aggregate three ready CI/harness PRs into one head: #2181 + #2177 + #2161" — this consolidation exists. | Validation. #2213 is the queue's closest to landable: review bound 1/1 at its exact head, needs only a receipt. |
| `land-rust-validate-migration-pr1635` | **REFUTED** | Its premise is "#1635 is open/conflicting/dirty — languishing". **#1635 is CLOSED.** The Rust validate is in production (`scripts/validate.rs` is the driver; the DAG files reference "after the Rust validate cutover"), and `port-forward-rust-validate-migration-1635` is CLOSED with `implemented`. | Nothing. Work completed by another route. |
| `soft_green_pr_drain` | **HOLDS, blocked** | Its own notes already record the devbig030 CPUID blocker. Verified today: both rebase records in `ignored/rebase-records.jsonl` self-report `landable: false — no-receipt-at-pushed-head`. | Validation capability. Needs devbig014; the receipt must bind the *pushed* head. |
| `revalidate_current_reverie_pin` | **UNCERTAIN** | PR **#1972 is MERGED**, so the triggering event is past; the pin gate is live and enforced. Whether the standing audit obligation persists is an owner reading, not a fact I can settle. | Validation capability, if a rerun is still wanted. |
| `compat_robust_futex_owner` | **HOLDS, overlaps 904** | Robust-futex appears on main only in `syscall_classification.rs` and `syscall_time.rs` — no owner-death modelling. Overlap confirmed: **#2223** is "Fix ptrace robust-futex owner-update race and harden evidence", but it is a **DRAFT** and narrower (owner-*update* race, not owner *death* on exit_group / de_thread / fatal signals). | Scope split with hermit-904, then validation. Nothing has landed. |
| `test_applications_e2e_stats` | **UNCERTAIN** | Not tested. Establishing shared-`ignored/` divergence needs a run, which this box cannot admit. | Validation capability. |

### The scheduler cluster — all four HOLD, and all four are genuinely unowned

All four came from adversarial review of PR #2071. **#2071 is MERGED** (`mergedAt`
2026-08-10T10:14:12Z, mergeCommit `d05d02d8`), so these are no longer findings against a
pending PR — they describe **code now on main**, which if anything raises them.

| task | premise | evidence on `origin/main` `1f4e430c1` |
| --- | --- | --- |
| `hermit_analyze_panics_instead` | **HOLDS** | `immediate_fatal_exit()` is still `std::process::exit(1)` — `detcore/src/scheduler.rs:1126-1128`. Drop still skipped, so `--record-preemptions-to` still writes no file for this class. |
| `make_an_unreachable_deadline` | **HOLDS** | `TimedEvents::pop()` (`timed_waiters.rs:312`) still returns `Option<(LogicalTime, TimedEvent)>` via `pop_if_before(LogicalTime::MAX)`, so it can still hand back an indefinite entry. `is_indefinite` remains a caller-checked predicate, not a type-level guarantee. |
| `rt_sigsuspend_and_pause` | **HOLDS** | The two models still differ: `pause` registers the `LogicalTime::INDEFINITE` sentinel (`scheduler.rs:5611`), while `rt_sigsuspend` has its own `handle_rt_sigsuspend` documented as running "without holding the deterministic scheduler turn" (`syscalls/signal.rs:218-223`). |
| `happens_before_checkpoints_are` | **UNCERTAIN** | `report_terminal_deadlock` still exists (`scheduler.rs:2650`) and the emptiness path is unchanged in shape, but establishing whether the *second* exception to the wakeup invariant is still reachable needs a reading of the happens-before checkpoint paths I did not do. Not refuted; not confirmed. |

**Ownership question answered:** all four are genuinely unowned in the database. `hermit-901`
holds `hermit_a_scheduler_deadlock` and `p0_privileged_e2e_manifest`, which are *different*
tasks. So these four need an owner; they are not merely awaiting a claim by an agent already
on them.

## What this changes about the board

- **Two premises are refuted** (`close_open_prs_whose`, `land-rust-validate-migration-pr1635`)
  and one is satisfied elsewhere (`fold_the_ci_harness`). Left open per instruction; the
  disposition is the owner's.
- **Nine premises hold**, and the four scheduler ones hold against *landed* code rather than a
  pending PR.
- **Four are blocked by validation capability alone** — they need devbig014, not an owner.
- **Three need no validation at all** and could be worked now by anyone free:
  `e2e_manifests_cannot_express`, `add_force_escape_to` Part B, and the scheduler cluster's
  type-level and exit-path work.
- **One tag is unbacked**: `add_force_escape_to` carries `implemented` while its Part B does
  not exist in the code. Landing debt rides on that tag, so an unbacked one misreports the
  debt.

## Commands

```sql
SELECT local_id,status,coalesce(owner,'-'),priority FROM tasks
 WHERE priority='P0' AND status!='CLOSED' AND (owner IS NULL OR owner='');
```

```sh
# is any open PR head already on main?
gh pr list --repo rrnewton/hermit --state open --json number,headRefOid   # then per head:
git -C hermit merge-base --is-ancestor <head> origin/main
# contradictory full-profile verdicts for one SHA
#   ledger/hermit/*/2026-08.jsonl -> group legacy_row.commit, collect exit_code==0 vs !=0,
#   restricted to rows carrying coverage.planned_test_nodes
grep -n "fn immediate_fatal_exit" -A3 hermit/detcore/src/scheduler.rs
grep -n "fn pop(" -A3 hermit/detcore/src/scheduler/timed_waiters.rs
grep -nE '"--force"' hermit/scripts/check-reverie-pin.rs
```
