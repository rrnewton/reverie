hermit-137 TOOK OVER THE WATCH FROM hermit-105. FIRST FINDING: THERE WAS NO RUN IN FLIGHT.

The handoff said a run was live and needed watching. It was not. The box was FREE at both
slots when I looked (2026-08-26 19:31 PDT), released at 19:28:04 by hermit-134.

WHAT ACTUALLY HAPPENED TO THE RUNS THAT WERE SUPPOSED TO PRODUCE THE ANSWER -- four attempts,
zero verdicts, and the two failure modes are different:

  1. TWO FULL RUNS WERE SIGTERMed MID-DAG AND ABANDONED. Both died in the SAME node,
     test.liteinst_strict, at almost the same wall time:
       hermit-134 @ c370c1371a15 : wall 1049s, CPU 4215s, exit 143, killed in test.liteinst_strict
       hermit-124 @ c85f795465b9 : wall 1109s, CPU 4196s, exit 143, killed in test.liteinst_strict
     Both logs read "supervisor received signal 15" -- the signal came from OUTSIDE the
     supervisor, not from a lease or a deadline. Both released the box and wrote
     "ABANDON ...: supervisor was signalled (15)". Neither produced a ledger row or a verdict.

  2. TWO MORE ATTEMPTS WERE REFUSED AT ADMISSION IN UNDER 22 SECONDS.
       hermit-134 @ c1b8c5db9c : exit 4, wall 13s and 22s, "validate REFUSED -- product
       validation inside dev-hermit must enter through canonical ci-hub admission."
     They invoked validate from a frozen-validate temp checkout rather than through
     ci-hub validate-run, so no lock was bound to the run and the front door refused.
     Cost of the two refusals: 1m53s and 1m54s of CPU each, for nothing.

WHY THE FIRST TWO DIED, AND IT IS NOT MYSTERIOUS. Their log filenames carry no start_unit
handle. They were run in the FOREGROUND of the launching agent's shell, so when that shell
call ended the process group took SIGTERM and the validate died with its caller.
ci-hub validate-run (start_unit.py) instead launches "a detached systemd user service", which
is exactly the property those two runs lacked. That is the whole difference, and it is why
17 and 18 minutes of a full DAG plus 70 CPU-minutes each were thrown away twice.

MY RUN, LAUNCHED THE OTHER WAY:
  unit    validate-hermit-137-a6b0c37648df-1787798057339452591-387681-9a8a881f.service
  log     ignored/validate/validate-hermit-137-a6b0c37648df-1787798057339452591-387681-9a8a881f.log
  target  a6b0c37648df774d5859aad23a535caf03a6d392   (hermit origin/main, re-verified unmoved
          immediately before launch)
  slot    validation slot 1, lease 3600s
  source  worktrees/h137val, fresh detached worktree, submodules recursive, 0 dirty paths,
          HEAD == target. No --run-on-dirty-tree; the freshness gate passed on its own, so
          this receipt does not carry that flag and does not need to be discounted for it.
  bounds  --child-deadline 12000 --wait 7200 --hold 3600
  policy  the two-attempts-per-cell retry from c370c1371a plus the append-not-overwrite retry
          records from c1b8c5db9c (#2708) are BOTH in this tree, so per-attempt elapsed times
          should survive this run rather than being overwritten.

CONDITION THAT WILL MATTER WHEN READING THE RESULT: I AM NOT ALONE ON THE BOX.
hermit-124 acquired slot 0 seven seconds before me and is validating 2bc359c755ee
concurrently. Two full DAGs are sharing 316 cores. The owner has ruled that failing under
contention is genuine flakiness to fix, so this does not invalidate the run -- but any cell
that dies at exactly the 15s per-test cap has to be read against the fact that a second full
validate was running beside it, which is the same contention hermit-107 measured hitting that
cap in 2 of 5 runs at 15.002s while completing in 2.5s otherwise.

IF I DISAPPEAR, THE RUN DOES NOT. Reattach with
  ./ci-hub/ci-hub validate-run --attach validate-hermit-137-a6b0c37648df-1787798057339452591-387681-9a8a881f
and read the TRAILING SUMMARY of the log -- the flaky-test warning fires even on a pass, so a
green exit is not by itself a clean run.
