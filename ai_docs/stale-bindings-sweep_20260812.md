# Stale-bindings sweep: records that outlived their referents

2026-08-12, ~20:40 PDT. Class-level sweep following five instances found one-at-a-time today,
each only when it blocked someone. Read-only: **nothing found here was cleared, released, or
tidied.** Ranked by blocking consequence, not by how interesting the defect is.

## Results, ranked

| # | binding | referent | state | blocking consequence |
| --- | --- | --- | --- | --- |
| 1 | validate-lock cleanup quarantine | payload PIDs 2487923, 2553747 | **both dead** | **HIGH — blocks all validate admission on devbig030 right now** |
| 2 | `locally-validated` label on #2176, #2108 | a qualifying receipt at that head | **no receipt exists** | **MEDIUM — misleads a reader; no re-verification in the read path** |
| 3 | 3 validate run records: `running`, `preparing`, `not-run` | a live systemd unit / process | **all inactive** | **NONE — downgraded, see below** |
| 4 | `landing-box.lock` anchor, `validate-box.lock` anchor | PIDs 968286, 3790780 | **both dead** | NONE — authoritative readers re-derive |
| 5 | obligations `producer_active` path | a live local producer | **no stale claim exists** | CLEAR — checked, 0 of 20 |
| 6 | 9 rebase records | PR heads | 7 of 9 reference only superseded heads | NONE — all self-report `landable=false` |

## 1. The quarantine (HIGH, still standing)

Recorded in full in
[devbig030-validate-admission-quarantine](devbig030-validate-admission-quarantine_20260812.md).
Both payload identities it names as "remain active" are dead by `/proc`; `validate-lock status`
agrees and still refuses, correctly, because the supervisor died before it could census its
domain. Fail-closed by design, with a documented `census-orphaned-domain` recovery whose
precondition is a by-hand cgroup proof. Not run, not cleared.

## 2. Two `locally-validated` labels with no receipt behind them (NEW)

```
PR 2176  head b3ca4e155098  labelled locally-validated  ->  validate-status: NEEDS-RERUN
PR 2108  head a9e8b457eceb  labelled locally-validated  ->  validate-status: NOT-VALIDATED
                                                            (no record at all)
```

This is the cleanest instance of the class in today's sweep: a label asserting a fact whose
referent does not exist. Two properties make it worth ranking above the run records:

- **Nothing re-derives it.** The run records below are re-verified by every consumer that reads
  them. A GitHub label is inert text; the only thing that can contradict it is a human or agent
  choosing to run `validate-status`.
- **It reads as reassurance.** `hermit-validation-authority` already says the label "is a
  convenience for people scanning a pull request, not evidence of its own. Never type or apply
  it by hand." The policy is right and the label still says the opposite of the truth to anyone
  skimming the queue.

It is not a *hard* blocker: the authoritative gate is `validate-status`, which refuses both
correctly, and both PRs are already recorded as unlandable in the
[drain evidence verdict](drain-evidence-verification_20260812.md). The hazard is a reader who
trusts the label instead of the gate. **Left in place** — the labels are the record of what was
believed, and stripping them is what destroyed #1874's review history on 2026-08-09.

## 3. The `running` run record — my own candidate, downgraded

I flagged this as a possible sixth blocker on the shape "a record asserting a live process that
may not exist", and asked whether a consumer treats `running` as "do not start another". Chased
to the end, **it does not hold.** There are exactly three consumers and every one of them
re-derives liveness rather than trusting the claim:

- **`run_registry.py`** has no directory scan at all. Every accessor is keyed by a specific unit
  name (`record_path`, `read_record`, `update_record`). Nothing enumerates the directory looking
  for in-flight runs, so a stale record cannot be counted as one.
- **`validate_lock.rs:1346`** requires the record to bind `state=launching|running` *for this
  agent and target*, and then requires the **live** systemd unit to equal the recorded unit. A
  stale record fails that comparison by construction.
- **`pane_lifecycle.py`** indexes records by `tab_id`, then calls `process_alive()`, which its
  own docstring describes exactly: "True only when THIS pid, on THIS boot, still has the
  recorded start time. A bare `kill(pid, 0)` would call a recycled pid alive and keep a dead
  run's slot forever, which is exactly the failure this module exists to end." It also applies
  an orphan TTL.

For the specific stale record, the boot check alone settles it:

```
record process_identity.boot_id : b971c325-38e4-44f5-80d2-70624d74da52
current /proc/sys/kernel/random/boot_id : 3ac87f92-e589-489d-9e37-bc1ab32d6a3e
```

The box has rebooted since. `process_alive()` returns false before it touches `/proc`.

So the record is stale, and it serialises nothing. Consequence: **NONE**. Recording the
downgrade rather than leaving an alarming flag standing — a candidate that survives only
because nobody chased it is the same defect class in a different costume.

## 4. Two stale box anchors (no consequence)

`landing-box.lock` names dead pid 968286 while `land-lock status` correctly reads `FREE`;
`validate-box.lock` names dead pid 3790780 from a run that completed. Both left as found; both
already recorded in the [lock premise audit](hermit-main-lock-premise-audit_20260812.md).

## 5. The `producer_active` path (checked, clear)

`VerificationState::receipt_disposition` (`ci-hub/lib/records.rs:56`) maps
`pending|starting|running` to `{"state":"producer_active","will_consume_later_receipt":true}`,
and it *is* consumed, by `ci-hub obligations` (`ci-hub.rs:6538`, `:6715`). A stale non-terminal
obligation would therefore tell a reader to wait for a receipt from a dead producer. Checked
the live store: `ignored/ci-hub/obligations.jsonl`, 59,429 lines, 20 latest-by-id, with
`local.state` distribution `{no_result: 15, green: 4, checkpoint: 1}`. **Zero** in a
non-terminal state. The mechanism is real; there is no instance of it today.

## 6. Rebase records (no consequence)

Nine records in `ignored/rebase-records.jsonl`; seven reference only superseded heads. All nine
self-report `landable: false`, so a reader is told the truth regardless of head drift.

## The pattern across the six

The instances that had consequence are the ones **nothing re-derives**: the quarantine (whose
refusal is deliberate) and the labels (which nothing recomputes). Every binding that a consumer
re-checks against `/proc`, a boot id, a start tick, or a live systemd unit turned out inert, no
matter how stale it looked. That suggests where to look for the seventh: not at records that
name a PID, but at **assertions no code recomputes** — labels, task tags, prose in a queue
declaration, a status field a human reads.

## Still to sweep

- `implemented`-tag task SHAs versus what is actually on the named target branch.
- Worktree registry rows versus live owners (partly covered by others earlier today).
- Directive ledger entries versus their claimed implementing commits (`check.py` exists for
  exactly this and was not run here).

## Commands

```sh
for f in /run/user/$(id -u)/ci-hub/*; do grep -oE 'pid=[0-9]+' "$f"; done   # then test /proc/<pid>
python3 -c "…"   # scan ignored/validate/runs/*.json for non-terminal state
./ci-hub/ci-hub validate-lock status
./ci-hub/ci-hub validate-status --sha <head-of-each-locally-validated-PR>
python3 -c "…"   # latest-by-id over ignored/ci-hub/obligations.jsonl, count local.state
cat /proc/sys/kernel/random/boot_id     # versus process_identity.boot_id in a run record
```
