# HANDOFF — allow_merge_gate_to — 2026-09-03, claude lane `demos-repair`

## Where the work actually is

**Not in this slot.** The trees are in `/home/newton/work/dev-hermit/worktrees/slots/demos`,
with Hermit as its `hermit/` submodule. That slot has no live agent (a
`DG_AGENT_NAME` sweep over `/proc/*/environ` shows no `demos`), so I worked in
place rather than copying and risking the uncommitted work it held. This slot
(`demos-repair`) holds only this handoff.

## Settled head — do not move it

- **Hermit `e877bfa06a175a0ef5bbda5328489680217a6a30`**, pushed to
  `refs/heads/move-demos-directory-into-hermit`, read back from GitHub as both
  the branch tip and the head of https://github.com/rrnewton/hermit/pull/2904
  (open, base `main`). Branch `demos/move-demos-directory-into-hermit-phase2`,
  clean. Supersedes `1eef513dc` → `d36a4c4ea` → `2f0fee1aa` → this, all
  **appends**. ⚠️ **Append only from here**: https://github.com/rrnewton/hermit/pull/2934
  is stacked on this branch with merge-base `2f0fee1aa`; a rewrite orphans it.
  Hosted run `33713130751` is green on all eight demos at this head.
- **Parent `6a55b3b3a8c9170317b347cb5509f3ba2853122d`** on
  `demos/move-demos-directory-into-hermit-parent`, unpushed as a branch,
  preserved at `refs/rescue/2026-09-03-6a55b3b3` with remote readback verified.
  Its `hermit` gitlink is **deliberately unstaged** at `6341e1e4`.
- The prior lane's candidate stays at `refs/rescue/2026-09-02-8c590598`.

## Round-three verdict status (read with the notes on `allow_merge_gate_to`)

| finding | state |
|---|---|
**Round 3** (at `1eef513d`): HIGH 1 Demo 08 fixed-control **fixed** `2b406b0c5`;
HIGH 3 comparator address identity **fixed** `2f0fee1aa`; LOW stale descriptions
**fixed** (incl. PR body — no guard fired); MEDIUM trailers filed
`two_demo_green_review`; **HIGH 2 Demo 2 release→debug still open**, filed as
`release_profile_replay_injects` (round 4 did not re-raise it).

**Round 4** (at `0fd1653f`, https://github.com/rrnewton/hermit/pull/2904#issuecomment-5520405599)
— **all three fixed**: Demo 08 calibration now requires the completed ASAN report
*and* rc=134 (`24b8d2a4f`); QMP socket relocated **only when over the AF_UNIX
bound** (`e877bfa06`, after my unconditional first attempt regressed the runner);
`demos/README.md` lists Demo 8 in all three inventories.

**Stale paths: 20**, re-derived at this head (rc=1, 108s). It went 18 → 19 → 20
as earlier fixes touched demo paths, then **held** — round 4's fixes added none,
since every `demos/**` file they touch was already in the set. One trailer naming
all 20 clears it. **The trailer is a reviewer's act; the author must not write it.**

Hosted at this head: run `33716857052`, **all 8 PASS** (81/4/4/4/33/78/9/46s).

⚠️ **Local green has twice not implied hosted green** in this lane — the socket
relocation and, earlier, the stale-anchor comparator. Dispatch Demo Hot Path
before trusting a demo change.

## Correction landed on the tip — read this first

The claim that Demo 2's release replay and Demo 08's runtime/calibration are
**owner-authorised known-red has no located source.** It appears only in this
workstream's own task notes and commit messages; the strongest form
("owner-known-red") was mine, and I have stripped it from the tip commit while
keeping every measurement. `5c990c6fb` and `7c4bd2551` still carry the weaker,
un-attributed form.

The evidence runs the other way for Demo 08: closed task
`demo-08-green-review-for-pr-2904` records an **independent** review finding it
GREEN ("positively not a skip"), and it passed on the runner in 47s and 44s.
Demo 2 **passes** — `demos/02-record-replay.sh:36` uses `target/debug/hermit`;
the release EFAULT is a real documented defect in a path the demo never runs.
The project's real mechanism is `ci-hub/validate/parity_gate.py` `KNOWN_RED`,
which holds backend parity pairs and neither demo. **Whether a directive exists
is the owner's to say — this is recorded, not resolved.**

If there is none, the 11/2/5 split below collapses to **18 resolvable** and
there is nothing to rule on.

## What is done

- **rustfmt: already resolved, and the remaining rc=1 must not be "fixed".**
  The canonical gate is `cargo fmt --all -- --check` (Makefile `lint-cargo`, CI
  node `lint.rustfmt`) and it is rc=0. The rc=1 is `rustfmt --check
  scripts/validate.rs`, which follows `mod` into `scripts/lib/` — 571 hunks over
  ten files, **byte-identical counts at the merge base** `2bd51fca`
  (410/61/18/18/16/15/15/11/6/1 both sides). Pre-existing drift in files this
  branch does not touch.
- **Gitlink ordering: dissolved, not deferred.** `origin/integration` is at
  `2bd51fca`, exactly this PR's base, and is an ancestor of the head — so
  advancing integration is a **fast-forward with no rebase and no SHA rewrite**.
  The parent bump still waits for *main*, because
  `ci-hub/remediation/demo_review_gitlink.py` requires the gitlink to be on
  Hermit `origin/main`.
- **Hosted evidence exists at the settled head.** Run `33706502124` at
  `1eef513dc`, conclusion **success**, all eight PASS with
  `DEMO08_REQUIRE_ASSETS=1` (99/4/3/4/37/36/9/44s). The pinned authority
  classifies it **PASSED**.
- **Gate contract proven live, both polarities.** Dispatched Demo Hot Path at
  each exact head. Run `33702888091` at `7c4bd2551` → **FAILED**; run
  `33704096613` at `69e921a0b` → **PASSED** (that tree is `e608d85c`, i.e. the
  current head's tree, so the evidence carries). Selection and classification were
  done with the gate's own pinned `ci-hub/check_outcome.py`
  (ref `4b78d727…`, sha256 `2f1c61d5…` verified), against real GitHub
  responses. While in flight the classifier returned NO_RESULT and
  `queue_hosted_retry` declined to duplicate. The producer's
  "Verify exact culprit attribution" step concluded success.
- **Parent landing authority raised to schema 4** (round-two finding #1),
  committed. Also closed a coverage hole it left — nothing mutated the
  *portable* identity — and replaced hand-maintained summary counts with
  execution-derived ones. 54 cases, 8 positive / 46 refused, rc=0 in 1.79s.
- **Fixed a false green in the demo sweep.** `demos/run-all.sh` recorded a demo
  that *declined to run* as PASS; demo8 skipped on missing assets and the sweep
  printed "all 8 requested demos passed". Now SKIP, with an INCOMPLETE headline,
  bracketed by `demos/tests/test_run_all_skip_classification.py`. Demo tests
  39 → 43, rc=0.

## What remains, and the blockers

1. **A distinct reviewer must run the touched demos at `1eef513dc`** and record
   `Demo-Green-Review:` trailers. `scripts/check-demo-review.sh --range
   2bd51fca..HEAD` is rc=1 with **18 stale paths** (the original 16 plus my two).
   I did not self-attest — I am the authoring lane.
2. **How many of the 18 need the owner depends entirely on the correction
   above.** Measured with a *dangling* probe commit (`git commit-tree`, no ref
   moved): a trailer naming 11 demo08-independent paths clears exactly 11,
   leaving 7 — the 5 Demo 08 paths plus `demos/Makefile` (its last change is to
   `process-prereqs`, which `demo8:` depends on) and `demos/run-all.sh` (drives
   all eight). Demo 08 sources **nothing**, so no shared library reaches it.
   **If no known-red directive exists, all 18 are simply attestable from a real
   run** and this item disappears. `prepare-demo08-assets.sh` still cannot
   configure *here* (`e2fsprogs-devel` absent, no `ext2fs.pc`), so Demo 08 must
   be run on the runner, where it passed in 47s and 44s.
3. **Merge Gate cannot be dispatched yet, repository-wide.** It refuses a
   definition it has not registered (`merge-gate.yml:539-557`,
   "a branch-owned gate cannot authorize itself"). `MERGE_GATE_V4_BLOB` is
   `1502659b…`; our blob is `a8c61c30…` **and main's is `8719f7b9…`** — the
   variable matches *no* main revision in the last twelve commits. Its three
   most recent runs all failed, newest 2026-08-25. Registering the blob is
   gate-lane work serialized with landing; **I did not touch the variable.**
4. **Demo 7: 1 hosted failure in 3** (FAIL 12s, PASS 9s, PASS 9s); local 2/2
   pass. That one failure is uncharacterised — a bare socket `timed out` with no
   phase, after the guest advance had already completed.
5. **Demo 6: 3/3 hosted passes, 1 local failure in 2 — and the failure is
   *diagnosed*, filed as `failed_newfstatat_renders_an` (P0).** Hosted passes do
   **not** show absence: the leak only surfaces when the residual bytes differ
   between a pair, so they show the residue matched. A failed `newfstatat` (ENOENT) makes detcore render a statbuf the
   kernel never wrote, leaking a stack address (`0x7fffffff97c0` vs `…97d0`)
   into DETLOG. Five independent hashes matched across the two runs — only
   Hermit's own log differed, so **the instrument invented the divergence**.
   Do **not** "fix" it by widening the demo's normalizer.
6. **A second sweep in the same tree is not comparable to the first.** `demo5:
   $(BOOT_SNAPSHOT)` is satisfied by the cached snapshot and its recipe only
   echoes, so demo5 reported PASS in **0s** on the rerun. A reviewer needs a
   cold tree, or demo5's green means nothing.

## Order

reviewer trailers → owner ruling on Demo 08 → advance `integration`
(fast-forward) → register the merge-gate blob and dispatch Merge Gate (last item
on this task) → parent bump to the landed main SHA.
