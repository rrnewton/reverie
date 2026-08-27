HANDOFF — agent(hermit-135), written at context exhaustion. Hermit main was
63b7786461 when I stopped. Nothing of mine is running except one `cargo check` in
worktree h135-cc, which is safe to kill; every other worktree is removed and every
branch I made is pushed.

⚠️ 2694 IS NOT MINE AND I RELEASED IT. agent(hermit-131) owns
https://github.com/rrnewton/hermit/pull/2694, rebased onto current main at
19e884d65fcf6b475a599aecb69ae78366193caf, description corrected, two review lanes
dispatched at that sha. My claim was a collision: I claimed the mergeable set
without checking who already held one. I made NO change to that pull request —
`gh-merge-verified` refused it before touching anything, and I have closed my task
as dropped.

════════════════════════════════════════════════════════════════
THE THREE YOU ASKED ABOUT
════════════════════════════════════════════════════════════════
All three were refused by `ci-hub/bin/gh-merge-verified`, which I did not
override. I did not review any of them on their merits — I only established WHY
each refused, at the head shown.

**2602 — https://github.com/rrnewton/hermit/pull/2602 — head bdf87b0dc9f2**
  Refused rc=5: a CHANGES-REQUEST names this EXACT head.
  NOT landable as it stands. It needs either the author to push a fix (the sha
  moves and the refusal stops naming the head) or the original reviewer to post
  `CHANGES-REQUESTED-WITHDRAWN-AT: <family> bdf87b0dc9f2...`. There is
  deliberately no override flag. Whoever takes it should read the changes-request
  first; I did not.

**2680 — https://github.com/rrnewton/hermit/pull/2680 — "pin the three matcher
narrowings that passed all seventeen tests"**
  Refused rc=6: a passed-review LABEL asserts an approval that is NOT BOUND to
  this head.
  ⚠️ POTENTIALLY LANDABLE, and cheaply — this is not a defect in the change, it is
  a missing marker. The label claims a review happened; nothing ties that claim to
  the head being merged. It needs a reviewer to post `APPROVED-AT: <family> <exact
  head> BY <agent>`. If the change is in fact sound, one review at the exact head
  unblocks it.

**2706 — https://github.com/rrnewton/hermit/pull/2706 — "make the retry ..."**
  Refused rc=6 for the same reason as 2680: passed-review label not bound to the
  head. Same remedy, same caveat — I did not read the diff.

⚠️ 2680 AND 2706 SHARE ONE MECHANISM WITH 2694, AND IT IS WORTH FIXING AS A CLASS
RATHER THAN THREE TIMES. A label asserts that review happened and nothing binds
that assertion to the head being merged, so the label reads as current approval
when it is not. That is the same shape as everything else I found tonight: a
recorded value that was true when written and reads as current. Three of the
twelve mergeable pull requests are blocked on it.

════════════════════════════════════════════════════════════════
WHAT I LANDED, SO NOBODY RE-LANDS IT
════════════════════════════════════════════════════════════════
  2721 -> 48136d9600   one line of ci/dag/portable.json (a node description)
  2637 -> a229c93fa6c0 a refused validate child is a could-not-evaluate
  2702 -> 5f98a2332f03 execute the validate refusal predicate in lint
  2696, 2705, 2719, 2720 merged 07:33–07:34
  2723 -> 63b7786461   make the two rust-scripts clippy-clean

⚠️ THE 2637 -> 2702 ORDER WAS LOAD-BEARING. 2702 installs a gate that is INERT
without 2637 underneath it, because 2637 supplies the classification the gate
checks. Verified on main after both landed: the gate evaluates SIXTEEN channel
examples, not zero. If either is ever reverted, revert both or the survivor is a
check that cannot fire.

⚠️ CONTENT VERIFICATION ON THE LAST FIVE IS WEAKER THAN "VERIFIED" SUGGESTS.
`gh-merge-verified` returned rc=3 on four of them — "check by hand, not that the
merge failed" — because it reads refs/pull/N/head and I had merged seconds
earlier. Re-checked after the refs settled: absent=0 on all five, so none is
definitely unlanded, but several report moved_on, which is undecidable from blobs.
2723 needed a hand check (arrived=0) and IS on main as 63b7786461 with its exact
subject and the same two files; the blob difference is the rebase replaying it
onto a main that had already taken the other four.

STILL OPEN, NOT ATTEMPTED: 2176, 2304, 2670, 2674, 2699 — all CONFLICTING, all
needing a rebase before any verdict means anything.

════════════════════════════════════════════════════════════════
⚠️ CARRY THIS STANDARD FORWARD — IT IS THE MOST TRANSFERABLE THING HERE
════════════════════════════════════════════════════════════════
On 2721 I wrote on the pull request that its one-directional landing test is
satisfied VACUOUSLY, because one line of a DAG node description cannot select,
add or remove a cell — rather than reporting a pass as though I had measured one.

A VACUOUS PASS DISCLOSED AS VACUOUS IS HONEST. THE SAME PASS REPORTED AS A
MEASUREMENT IS HOW THIS PROJECT ACCUMULATES CHECKS THAT CANNOT FAIL. Whoever picks
these up should hold that line: when a rule is satisfied because nothing could
have violated it, say so in those words.

Three more instances of the same discipline, in case they save someone a repeat:
  - A STALE PROBE'S FAILURE DESCRIBES THE PROBE. I refused to cite my own
    ci-hub/bin/refusal-predicate-gate-probes against 2702 because those probes
    assert an older contract; their three failures were mine, not the gate's.
    They still need updating to pass a returncode.
  - A MUTATION THAT DID NOT APPLY IS WORTHLESS. My first attempt to prove the
    2702 gate load-bearing made ZERO substitutions and appeared to show a hole.
    I verified the substitution was present before trusting the second result.
  - READING OUTPUT THROUGH `tail -1` HID A REFUSAL FROM ME FOUR TIMES: the last
    line of tg-note-verified's refusal is the word EOF from its own usage example,
    which reads like success. Capture the output and test rc; do not tail it.

════════════════════════════════════════════════════════════════
OPEN THREADS I OWNED, WITH THEIR STATE
════════════════════════════════════════════════════════════════
  - Goal 1 is RESOLVED as unsatisfiable-as-written and closed: all 371 red cells
    are not-selected, zero diverged, so no wiring can produce a coordinate. Red
    means NOT IN THE GATED SET for 288 of 371, not "fails".
  - The diverging population is AT LEAST EIGHT, not seven, and not bounded above.
    hardware-trap-identity/dbt diverges 3 of 3 while recorded infrastructure-error,
    so the DBT log-retention condition HIDES a reproducible divergence.
  - Three coordinates are DRIFTED and must be replaced not imported; sent to
    agent(hermit-127). Retained batch summaries preserved at
    /home/newton/work/dev-hermit/ignored/hermit-135-*.json.
  - `every_reverie_bump_silently` is open and unowned: every Reverie pin bump
    silently disables the DBT/SaBRe build node until expected_pin in
    ci/run-with-reverie-dbt-budget.sh is recalibrated by hand.

════════════════════════════════════════════════════════════════
REVALIDATE LOOP — LAUNCHED, VERDICT NOT YET AVAILABLE
════════════════════════════════════════════════════════════════
Standing catch-up obligation, not a one-off. Main was NOT-VALIDATED in the ledger.

  target sha : fa76f4e66b763b74d428464907c21457f37ed7f1
  slot       : worktrees/h135-val, detached at that sha, tracked-dirty 0
  service    : validate-hermit-135-fa76f4e66b76-1787817995110591928-1768041-bbc61201.service
  log        : ignored/validate/validate-hermit-135-fa76f4e66b76-1787817995110591928-1768041-bbc61201.log
  state      : active; currently yielding in validate-lock for owner watch
               ("no durable owner-watch admission since release by hermit-138")

⚠️ I CANNOT REPORT A VERDICT. A full validate runs for hours and I am at context
exhaustion. It runs as a systemd service so it survives my turnover; whoever picks
this up should read the verdict from
  ci-hub validate-status --sha fa76f4e66b763b74d428464907c21457f37ed7f1 --repo rrnewton/hermit
and NOT from the log tail.

⚠️ FIRST ATTEMPT REFUSED AND THE REFUSAL WAS CORRECT: I targeted e0ec80114b25 and
got "validation target does not contain freshly fetched origin/main. Rebase before
validating." Main had moved between my reading it and the launch. Relaunched at
fa76f4e66b. If the next reader finds this target stale too, main moved again —
re-fetch and relaunch rather than validating a sha nobody is on.

⚠️ AND THE OWNER-WATCH YIELD IS NOT PROGRESS. The run is alive but repeatedly
yielding the validation slot; if it is still yielding when someone checks, it has
not started measuring anything and the absence of a verdict is not a red.
