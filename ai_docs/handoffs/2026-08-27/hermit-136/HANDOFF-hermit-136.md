# Handoff — agent(hermit-136), 2026-08-27 ~08:30Z

Everything below is either landed and verified from the remote, or explicitly
marked as not done. Nothing is uncommitted; no branch is unpushed.

## 1. THE MERGE WINDOW — CLOSED. DO NOT MERGE THE PARENT YET.

The batched operation (fast-forward merge, then `python3
ci-hub/series/pressure_schedule.py launch`, then read the first result) is
AUTHORISED by the coordinator but gated on a quiet box. It was still closed at
handoff: five validate units running (hermit-120, 125, 129, 135, 138).

⚠️ **What `start_unit.py` having live processes means, because this is the whole
reason the operation is batched.** `ci-hub/validate/start_unit.py` is
simultaneously (a) one of the paths the pending merge would replace and (b) the
script every in-flight validate is executing, by absolute path, from the shared
parent. Merging while it has live processes swaps the validate starter under a
running validate. The run does not crash — git replaces by rename, so an open
inode survives — but the NEXT invocation inside the same logical run gets
different code. That is how a run gets split across two code versions and
produces an inexplicable failure nobody traces back to the merge.

The check: `ignored/hermit-136/merge-window-check.sh`, exit 0 open / 1 closed /
2 unknown. It derives the file set from `git diff --name-only HEAD origin/main`
so it cannot go stale against a guessed name list, and it counts running systemd
validate units, which no path check sees.

⚠️ **TWO DEFECTS IN THAT CHECK, BOTH MINE, BOTH UNFIXED.**
  1. IT CAN HANG. It completed in seconds earlier and TIMED OUT AT 10 MINUTES at
     handoff, almost certainly the `lsof` sweep over a grown 46-path incoming
     set while five validates run. A check that can hang is a check whose
     silence means two things. Bound its work — cap the per-path `lsof`, or drop
     `lsof` and keep the argv scan, which is what actually caught every real
     case.
  2. A PASS IS A SNAPSHOT, NOT A GUARANTEE. Unit counts moved 4 -> 1 -> 3 -> 2
     across single minutes. When it does exit 0, re-run it IMMEDIATELY before
     the merge and report both readings, so the merge does not land in the gap
     between two unit starts.

⚠️ Do not use the validate lock as the quiescence signal. Measured: it reported
one held slot while 23 processes were executing parent code. It governs
validation ADMISSION and is correct for that — see item 5 — but it is silent
about self-tests and rust-script tests.

## 2. DBT PREFIX FIX — OPEN AS A PULL REQUEST, NOT MERGED

https://github.com/rrnewton/hermit/pull/2724, head 53ecdac33868376c384dad19bf1de84a1d2faaa5,
verified from the remote by sha. Built and tested at hermit main 814b8e0f49.

`ci/compat-envelope/pressure-test.rs` and `ci/manifest-plan/src/runner.rs` both
scan for `run1_log_` / `run2_log_`. `run.rs` passed "run1"/"run2" for every
backend; `backends.rs:793` passed "dbt-run1"/"dbt-run2", producing
`dbt-run1_log_`. Measured with the backend as the only variable: dbt wrote both
captures, 47,507 bytes each, predicate matched 0 of 2; the same command without
`--backend dbt` matched 2 of 2. Fixed on the PRODUCER — the scanners already
refuse missing/duplicate/empty captures and are untouched.

WHAT IT MOVES, projected from verdicts ALREADY RECORDED (all 59 cells carry this
evidence error and no other):
    before  59 cells `infrastructure-error`
    after    2 real FAILs — backend-parity-c/hardware-trap-identity (diverged 3
             of 3) and language-runtimes/example-python-random (diverged 1 of 2)
            57 passes
⚠️ Projected, not observed. The compat-envelope harness should confirm.

Regression 92 passed / 12 failed against base 814b8e0f49 at 91/12 with a
byte-identical failure-name set; the +1 is the new test.

⚠️ NOT FIXED BY THAT PR, worth its own task: the 8 cells named in the original
task carry a reason text ("DBT ... returns without a terminal canonical
comparison, leaving verdict=no_result and comparison=null") that is FALSIFIED —
they produced terminal verdicts 24 of 24 — and an evidence pointer at
`run.rs:2163` citing unrelated shebang/chmod code. A wrong reason with a
specific-looking citation survives scrutiny better than no reason.

## 3. PARENT RECONCILE — 33 OF 66 POLICED PATHS STALE AND ACCELERATING

4 of 30 → 19 of 50 → 33 of 66 over roughly three hours. Half the policed surface
now differs from origin/main, including `ci-hub/bin/changes-requested-gate`,
`ci-hub/bin/refusal-predicate-gate-probes` and `ci-hub/health/approval_binding.py`.

⚠️ The consequence the coordinator named and I agree with: every tick warning
acted on tonight came from code that is not what landed. A gate fixed on main
may still be firing its old behaviour here; a gate added on main may not run at
all.

The cause is NOT dirt and NOT divergence — established twice. It is that nothing
fast-forwards the parent, deliberately: `scripts/primary_checkout.py:1552`
reports it and never performs it, because "a fast-forward moves the working tree
of a shared integration surface with live sibling processes". The remedy it
names is gated on quiescence, and quiescence is not currently detectable.

⚠️ Do NOT automate this on a timer. That was assessed and declined; the
reasoning is on task `the-parent-checkout-goes-stale-again-within-the-hour-so-a-one-time-reconcile-is-not-the-fix`,
including why a landing-agent variant improves ownership but not safety. The
landed mitigation is `ci-hub/health/validate_run_code_currency.py` (a182cec70),
which records the tooling commit per run so a split run stops being invisible.

## 4. h135-cc VERSUS h135-val — ONLY ONE IS REMOVABLE

⚠️ **`worktrees/h135-val` IS IN ACTIVE USE. DO NOT REMOVE IT.** Four live
processes name it as `--checkout`, including the supervising `start_unit.py`
pid 1768041, which is the pid embedded in the running unit
`validate-hermit-135-fa76f4e66b76-...-1768041-bbc61201.service` holding slot 0.
Its run record says `state: running`. Snapshot publication stays blocked until
that run finishes; that is the correct trade.

⚠️ TWO CHECKS THAT WOULD HAVE SAID IT WAS SAFE, AND BOTH ARE WRONG HERE:
  * `cwd` / `root` / `exe` gave **0 references**. A validate does not hold its
    checkout as a cwd; it passes it to a detached unit. THE DETECTOR IS ARGV.
  * The run record's own `checkout` field says
    `ignored/validate-fresh-yL5JJX25` — a derived fresh tree, NOT h135-val.
    Trusting the record would have concluded the directory was unused.

`worktrees/h135-cc` (1.6G) is reported removable by agent(hermit-135)'s handoff —
a read-only compile check that confirmed main builds in 1m03s. ⚠️ I DID NOT
VERIFY THAT MYSELF. Before removing it, run the argv check above for `h135-cc`
as well as cwd/exe/root, and check for unpushed commits and dirty files. My
cwd/exe/root scan showed 0 for it, but that is exactly the detector that was
insufficient for h135-val.

## 5. SENT TO agent(hermit-122), on its task
`eliminate-the-shared-path-contentions-so-concurrent-validates-are-safe`:
how to read genuine slot availability. Key points: enter the FIFO queue via
`acquire` rather than polling for a gap and racing; hermit-122 already has an
`owner-directive` declaration queued; and ⚠️ in
`validate-lock authority-status --json` the per-slot field `admissible` is TRUE
for the HELD slot and FALSE for the FREE one — key on `state`, not `admissible`.

## 6. SMALLER ITEMS
  * `scripts/slot-owner-authority.py --all --json` emits a jemalloc warning on
    STDOUT ahead of the JSON. `json.load(stdout)` raises. Not fixed.
  * Preserved tonight and recorded on their tasks: 160 rescue refs covering 404
    commits from /tmp worktrees, 150 dropped-stash refs, 28 uncommitted-edit
    patches, plus 2 unpushed commits and 1 patch from unregistered slot dirs.
    Manifests under `ai_docs/evidence/tmp-worktree-unpushed-inventory-20260827/`
    and `ai_docs/evidence/tmp-uncommitted-capture-20260827/`. Both are UNTRACKED
    in the parent; someone should commit them.
  * My tools live in `ignored/hermit-136/`: `merge-window-check.sh`,
    `stability-by-attempt-counts.py`, `pressure-test-stale-or-reverted-ANSWER.md`,
    and the dbt/ptrace log-dir probe directories kept as evidence.
  * reverie #411 approved at 78c3b1a36892e40e882a2e62fecc82d786b77ef0 with an
    explicit STATIC-review caveat on the PR; the landing decision is
    agent(hermit-132)'s.
