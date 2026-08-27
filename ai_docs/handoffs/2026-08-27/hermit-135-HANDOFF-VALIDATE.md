HANDOFF ADDENDUM — agent(hermit-135), written at context exhaustion.
⚠️ THE VALIDATE RUN IS THE BLOCKING ITEM. Everything else in
ignored/hermit-135-HANDOFF.md still stands.

════════════════════════════════════════════════════════════════
1. THE RUN THAT IS HOLDING A BOX SLOT
════════════════════════════════════════════════════════════════
WHAT IT IS. A full validate I launched against hermit main under the standing
revalidate obligation, because the ledger said that sha was NOT-VALIDATED.

  target sha : fa76f4e66b763b74d428464907c21457f37ed7f1
  service    : validate-hermit-135-fa76f4e66b76-1787817995110591928-1768041-bbc61201.service
  state      : ACTIVE
  log        : ignored/validate/validate-hermit-135-fa76f4e66b76-1787817995110591928-1768041-bbc61201.log
  launched   : 01:06, elapsed about 10.5 minutes as I write this

IS IT MAKING PROGRESS? YES, and this changed since my last report. It is no longer
yielding in validate-lock. The log is being written continuously (mtime is current
to the second) and it is inside the DAG, running build and check nodes:

    [build.workspace]      … 30s elapsed, no test events yet
    [check.script_sigpipe] … 30s elapsed, no test events yet

So it is past admission and executing. My earlier note that it was "yielding, not
progressing" is SUPERSEDED — that was true at 01:02 and is not true now.

HOW LONG LEFT. I do not know, and I will not invent a figure. What I can say: it
is about 10 minutes in and still in the BUILD phase, before the e2e cell phase. A
full validate on this box has been described elsewhere tonight as multi-hour, and
the 336-cell verify batches I ran took roughly 20 minutes EACH on top of a build.
Treat "hours, not minutes" as the shape and read the real answer from the ledger
rather than from me:

    ci-hub validate-status --sha fa76f4e66b763b74d428464907c21457f37ed7f1 --repo rrnewton/hermit

⚠️ IT IS NOT THE SLOT-0 HOLDER, WHICH MATTERS FOR agent(hermit-122). My own log
records the holder it queued behind as `slot 0: pid=915529`, which is NOT any of
my processes. My three are 1693353 (the ci-hub wrapper), 1693374 (ci-hub.rs) and
1768010 (start_unit.py). So freeing goal 5's concurrency pair may need pid 915529
dealt with as well as my run — please confirm the current holder rather than
assuming killing mine clears both slots.

IF IT SHOULD BE STOPPED, that is a legitimate call — I am gone and cannot report
its verdict. Stop it cleanly with
    systemctl --user stop validate-hermit-135-fa76f4e66b76-1787817995110591928-1768041-bbc61201.service
and expect NO receipt: a stopped validate writes no canonical result, and the sha
simply stays NOT-VALIDATED. That is an honest outcome, not a red.

════════════════════════════════════════════════════════════════
2. ⚠️ worktrees/h135-val IS THAT RUN'S WORKTREE. DO NOT REMOVE IT YET.
════════════════════════════════════════════════════════════════
YES, IT IS IN USE. All three live processes above carry
`--checkout /home/newton/work/dev-hermit/worktrees/h135-val` on their command
lines. Removing that directory while the run is active would destroy the checkout
underneath a running validate and produce a failure that looks like a product
red rather than a removed tree.

CORRECT ORDER IF IT MUST GO:
  1. systemctl --user stop <the service above>
  2. confirm nothing remains: pgrep -af h135-val
  3. git -C hermit worktree remove --force worktrees/h135-val && git -C hermit worktree prune

It was created clean and detached at fa76f4e66b with tracked-dirty 0, so it holds
no work of mine — only the validate's own build output. Nothing is lost by
removing it once the run is stopped.

⚠️ ONE OTHER DIRECTORY: worktrees/h135-cc had a `cargo check --workspace
--all-targets` running when I last looked. It holds nothing — it was a read-only
compile check confirming main builds, which it did (Finished in 1m 03s). Kill the
cargo and remove it freely.

════════════════════════════════════════════════════════════════
3. WHAT A SUCCESSOR NEEDS
════════════════════════════════════════════════════════════════
- The revalidate obligation is STANDING, not discharged. If my run is stopped, the
  next agent inherits it. Re-fetch first: main moved under me twice while I was
  launching, and my first attempt was correctly REFUSED with "validation target
  does not contain freshly fetched origin/main". Validate the sha people are
  actually on.
- Read verdicts from the ledger, never from a log tail.
- Just landed by me and worth not re-doing: 2637 -> 2702 in that order (2702's
  gate is inert without 2637 underneath; verified non-vacuous on main, 16 channel
  examples). Also 2721, 2696, 2705, 2719, 2720, 2723.
- Just reviewed by me: 2216 APPROVED-AT claude 9c835cf5d22e907448bcd287d59d113a521ad999.
  That change is the FIX for the label-not-bound-to-head refusals currently
  blocking 2680, 2706 and 2694 — landing it should unblock that class.
- 2694 is agent(hermit-131)'s, not mine; I released it untouched.
- Still refused and needing a marker or a fix, not adoption: 2602 (changes-request
  at the exact head), 2680 and 2706 (label not bound to head).
